use super::helpers::*;

use mahler::dag::{Dag, par, seq};
use serde_json::json;

#[test]
fn it_finds_a_workflow_to_create_networks() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
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
                                "service1": {
                                    "id": 1,
                                    "started": true,
                                    "image": "ubuntu:latest",
                                    "config": {
                                        "networks": {
                                            "my-network": {}
                                        }
                                    },
                                },
                            },
                            "networks": {
                                "my-network": {},
                            },
                        }
                    }
                }
            },
        }),
        // The service references 'my-network', so 'setup network' must complete
        // before 'install service' can run.
        seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "setup network 'my-network' for app 'my-app-uuid'",
                "initialize service 'service1' for release 'my-release-uuid'",
            )
            + seq!(
                "pull image 'ubuntu:latest'",
                "install service 'service1' for release 'my-release-uuid'",
                "start service 'service1' for release 'my-release-uuid'",
                "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            ),
    );
}

#[test]
fn it_finds_a_workflow_to_remove_networks() {
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
                            "services": {},
                            "networks": {
                                "old-network": {},
                            },
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
                            "services": {},
                        }
                    }
                }
            },
        }),
        seq!("remove network 'old-network' for app 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_to_create_and_remove_networks() {
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
                            "services": {},
                            "networks": {
                                "network-a": {},
                            },
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
                            "services": {},
                            "networks": {
                                "network-b": {}
                            },
                        }
                    }
                }
            },
        }),
        par!(
            "remove network 'network-a' for app 'my-app-uuid'",
            "setup network 'network-b' for app 'my-app-uuid'",
        ),
    );
}

// Network config updates are handled by removing the old network
// and creating it with the new config
#[test]
fn it_finds_a_workflow_for_updating_networks() {
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
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "oci": running_container("my-release-uuid_my-service"),
                                    "config": {
                                        "networks": {
                                            "my-network": {}
                                        }
                                    },
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "oci_name": "my-network_my-app-uuid",
                                    "config": {
                                        "driver": "bridge",
                                        "enable_ipv6": false,
                                    },
                                },
                            },
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
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "config": {
                                        "networks": {
                                            "my-network": {}
                                        }
                                    },
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "config": {
                                        "driver": "overlay",
                                        "enable_ipv6": true,
                                    },
                                },
                            },
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
                "uninstall service 'my-service' for release 'my-release-uuid'",
            ) + par!(
                "remove network 'my-network' for app 'my-app-uuid'",
                "initialize service 'my-service' for release 'my-release-uuid'"
            ) + seq!(
                "setup network 'my-network' for app 'my-app-uuid'",
                "install service 'my-service' for release 'my-release-uuid'",
                "start service 'my-service' for release 'my-release-uuid'",
            ),
        ) + seq!("release locks for app with uuid 'my-app-uuid'"),
    );
}

#[test]
fn it_finds_a_workflow_to_create_multiple_networks_and_finalizes_release_after_network_create() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
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
                            "services": {},
                            "networks": {
                                "net-a": {},
                                "net-b": {},
                            },
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "setup network 'net-a' for app 'my-app-uuid'",
                "setup network 'net-b' for app 'my-app-uuid'",
            )
            + seq!("finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",),
    );
}

#[test]
fn it_uninstalls_services_using_a_legacy_network_before_removing_it() {
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
                                "my-service": {
                                    "id": 1,
                                    "image": "ubuntu:latest",
                                    "started": true,
                                    "oci": running_container("my-release-uuid_my-service"),
                                    "config": {
                                        // legacy containers reference the network
                                        // by its engine name
                                        "networks": {
                                            "my-app-uuid_my-network": {}
                                        }
                                    },
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "oci_name": "my-app-uuid_my-network",
                                    "config": {
                                        "driver": "bridge",
                                    },
                                },
                            },
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
                    "name": "my-app",
                    "releases": {
                        "my-release-uuid": {
                            "installed": true,
                            "services": {},
                        }
                    }
                }
            },
        }),
        seq!(
            "take locks for app with uuid 'my-app-uuid'",
            "stop service 'my-service' for release 'my-release-uuid'",
            "uninstall service 'my-service' for release 'my-release-uuid'",
            "release locks for app with uuid 'my-app-uuid'",
            "remove network 'my-network' for app 'my-app-uuid'",
        ),
    );
}

#[test]
fn it_removes_a_legacy_network_instead_of_migrating_it() {
    init_tracing();
    assert_workflow(
        json!({
            "uuid": "my-device-uuid",
            "apps": {
                "my-app-uuid": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "old-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-network": {
                                    // legacy networks are namespaced by prefix
                                    "oci_name": "my-app-uuid_my-network",
                                    "config": {
                                        "driver": "bridge",
                                    },
                                },
                            },
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
                        "new-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-network": {
                                    "config": {
                                        "driver": "bridge",
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        // the new network is created under the app namespace, so the legacy one
        // must be removed from the engine rather than left for adoption
        par!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "remove network 'my-network' for app 'my-app-uuid'",
        ) + par!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "setup network 'my-network' for app 'my-app-uuid'",
        ) + seq!("finish release 'new-release' for app with uuid 'my-app-uuid'"),
    );
}
