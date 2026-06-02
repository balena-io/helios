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
                                    "started": true,
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "config": {},
                                },
                                "service3": {
                                    "id": 3,
                                    // repeated image should only generate one pull
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "config": {},
                                },
                                "service4": {
                                    "id": 4,
                                    // different image same digest
                                    "image": "registry2.balena-cloud.com/v2/deafc41f@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "config": {},
                                },
                                // additional images to test download batching
                                "service5": {
                                    "id": 5,
                                    "image": "alpine:latest",
                                    "started": true,
                                    "config": {},
                                },
                                "service6": {
                                    "id": 6,
                                    "image": "alpine:3.20",
                                    "started": true,
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
                "initialize service 'service6' for release 'my-release-uuid'",
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
                "install service 'service6' for release 'my-release-uuid'",
            )
            + par!(
                "start service 'service1' for release 'my-release-uuid'",
                "start service 'service2' for release 'my-release-uuid'",
                "start service 'service3' for release 'my-release-uuid'",
                "start service 'service4' for release 'my-release-uuid'",
                "start service 'service5' for release 'my-release-uuid'",
                "start service 'service6' for release 'my-release-uuid'",
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
                                    "started": true,
                                    "oci": running_container("old_container"),
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
                    "oci_id": "abcde",
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
                "take locks for app with uuid 'my-app-uuid'",
                "stop service 'my-service' for release 'my-release-uuid'",
                "remove container for service 'my-service' for release 'my-release-uuid'",
                "install service 'my-service' for release 'my-release-uuid'",
                "start service 'my-service' for release 'my-release-uuid'",
            ),
        ) + seq!("release locks for app with uuid 'my-app-uuid'"),
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
                                    "started": true,
                                    "oci": running_container("my-release-uuid_service1"),
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "oci": running_container("my-release-uuid_service2"),
                                    "config": {},
                                },
                            }
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080" : {
                    "oci_id": "abcde",
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
            "take locks for app with uuid 'my-app-uuid'",
            "stop service 'service2' for release 'my-release-uuid'",
            "uninstall service 'service2' for release 'my-release-uuid'",
            "release locks for app with uuid 'my-app-uuid'",
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
                                    "started": true,
                                    "oci": running_container("my-release-uuid_service1"),
                                    "config": {},
                                },
                                "service2": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080",
                                    "started": true,
                                    "oci": running_container("my-release-uuid_service2"),
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "oci_name": "my-app-uuid_my-network",
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
                    "oci_id": "dfe123",
                    "download_progress": 100,
                },
                "registry2.balena-cloud.com/v2/deafbeef@sha256:4923e45e976ab2c67aa0f2eebadab4a59d76b74064313f2c57fdd052c49cb080" : {
                    "oci_id": "abcde",
                    "download_progress": 100,
                }
            }
        }),
        json!({
            "uuid": "my-device-uuid",
            "apps": {},
        }),
        seq!(
            "remove network 'my-network' for app 'my-app-uuid'",
            "take locks for app with uuid 'my-app-uuid'",
            "stop service 'service1' for release 'my-release-uuid'",
            "uninstall service 'service1' for release 'my-release-uuid'",
            "stop service 'service2' for release 'my-release-uuid'",
            "uninstall service 'service2' for release 'my-release-uuid'",
            "remove release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "release locks for app with uuid 'my-app-uuid'",
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
                                    "started": true,
                                    "oci": running_container("my-release-uuid_one"),
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
                                    "oci": running_container("my-release-uuid_two"),
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
                                    "started": true,
                                    "config": {},
                                },
                                "two": {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/deafbeef@sha256:b111111111111111111111111111111111111111111111111111111111111111",
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
                                    "started": true,
                                    "oci": running_container("old-release_service1"),
                                    "config": {},
                                },
                                // so is this service, however is not currently running
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222",
                                    "started": true,
                                    "oci": stopped_container("old-release_service2"),
                                    "config": {},
                                },
                                // this service should be migrated
                                "service3":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc2@sha256:a333333333333333333333333333333333333333333333333333333333333333",
                                    "started": true,
                                    "oci": running_container("old-release_service3"),
                                    "config": {},
                                },
                                // this service is being removed
                                "service4a":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/oldsvc4@sha256:a444444444444444444444444444444444444444444444444444444444444444",
                                    "started": true,
                                    "oci": running_container("old-release_service4a"),
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
                    "oci_id": "111"
                },
                "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "222"
                },
                "registry2.balena-cloud.com/v2/oldsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "333"
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
                                    "started": true,
                                    "config": {},
                                },
                                "service2":  {
                                    "id": 2,
                                    "image": "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                    "started": true,
                                    "config": {},
                                },
                                "service3":  {
                                    "id": 3,
                                    // same image hash as the service from the old release
                                    "image": "registry2.balena-cloud.com/v2/newsvc3@sha256:a333333333333333333333333333333333333333333333333333333333333333",
                                    "started": true,
                                    "config": {},
                                },
                                // this is a new service
                                "service4b":  {
                                    "id": 3,
                                    "image": "registry2.balena-cloud.com/v2/newsvc4@sha256:b444444444444444444444444444444444444444444444444444444444444444",
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
            + par!(
                // all the operations below can happen concurrently
                // uninstalls cannot happen until all images have been downloaded
                "install service 'service1' for release 'new-release'",
                "install service 'service2' for release 'new-release'",
                "install service 'service4b' for release 'new-release'",
                "uninstall service 'service2' for release 'old-release'",
            )
            + seq!(
                "take locks for app with uuid 'my-app-uuid'",
                "stop service 'service1' for release 'old-release'",
                "uninstall service 'service1' for release 'old-release'"
            )
            + dag!(
                seq!("start service 'service1' for release 'new-release'"),
                seq!("start service 'service2' for release 'new-release'"),
                seq!("start service 'service4b' for release 'new-release'"),
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
                "update image metadata for service 'service3' of release 'new-release'"
            )
            + seq!(
                "finish release 'new-release' for app with uuid 'my-app-uuid'",
                "release locks for app with uuid 'my-app-uuid'"
            ),
    );
}

#[test]
fn it_orders_service_start_by_depends_on() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": { "my-app-uuid": { "id": 1, "name": "my-app" } },
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
                                "db": {
                                    "id": 1,
                                    "image": "alpine:latest",
                                    "started": true,
                                    "config": {},
                                },
                                "web": {
                                    "id": 2,
                                    "image": "alpine:latest",
                                    "started": true,
                                    "depends_on": {
                                        "db": {"condition": "service_started", "restart": false, "required": true}
                                    },
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
                "initialize service 'db' for release 'my-release-uuid'",
                "initialize service 'web' for release 'my-release-uuid'",
            )
            + seq!("pull image 'alpine:latest'")
            + par!(
                "install service 'db' for release 'my-release-uuid'",
                "install service 'web' for release 'my-release-uuid'",
            )
            // The installs parallelize, but the starts are serialized indicating
            // service_started dependency being enforced.
            + seq!(
                "start service 'db' for release 'my-release-uuid'",
                "start service 'web' for release 'my-release-uuid'",
            )
            + seq!("finish release 'my-release-uuid' for app with uuid 'my-app-uuid'"),
    );
}
