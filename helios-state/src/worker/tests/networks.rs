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
                                    "container_name": "my-release-uuid_service1",
                                    "image": "ubuntu:latest",
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
                                },
                            },
                        }
                    }
                }
            },
        }),
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
                                "network-b": {
                                    "network_name": "my-app-uuid_network-b",
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
            par!(
                "remove network 'network-a' for app 'my-app-uuid'",
                "setup network 'network-b' for app 'my-app-uuid'",
            ),
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
                            "services": {},
                            "networks": {
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
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
                                "my-network": {
                                    "network_name": "my-app-uuid_my-network",
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
                "remove network 'my-network' for app 'my-app-uuid'",
                "setup network 'my-network' for app 'my-app-uuid'",
            ),
        ),
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
                                "net-a": {
                                    "network_name": "my-app-uuid_net-a",
                                },
                                "net-b": {
                                    "network_name": "my-app-uuid_net-b",
                                },
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
