use super::helpers::*;

use mahler::dag::{Dag, par, seq};
use serde_json::json;

#[test]
fn it_finds_a_workflow_to_create_volumes() {
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
                            "volumes": {
                                "my-volume": {
                                    "volume_name": "my-app-uuid_my-volume",
                                },
                            },
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "initialize service 'service1' for release 'my-release-uuid'",
                "setup volume 'my-volume' for app 'my-app-uuid'",
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
fn it_finds_a_workflow_to_remove_volumes() {
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
                            "volumes": {
                                "old-volume": {},
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
        seq!("remove volume 'old-volume' for app 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_to_create_and_remove_volumes() {
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
                            "volumes": {
                                "volume-a": {},
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
                            "volumes": {
                                "volume-b": {
                                    "volume_name": "my-app-uuid_volume-b",
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
                "remove volume 'volume-a' for app 'my-app-uuid'",
                "setup volume 'volume-b' for app 'my-app-uuid'",
            ),
        ),
    );
}

// Volume config updates are handled by removing the old volume
// and creating it with the new config
#[test]
fn it_finds_a_workflow_for_updating_volumes() {
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
                            "volumes": {
                                "my-volume": {
                                    "volume_name": "my-app-uuid_my-volume",
                                    "config": {
                                        "driver": "local",
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
                            "volumes": {
                                "my-volume": {
                                    "volume_name": "my-app-uuid_my-volume",
                                    "config": {
                                        "driver": "local",
                                        "driver_opts": {
                                            "type": "nfs",
                                            "o": "addr=10.0.0.1,rw",
                                            "device": ":/export/data"
                                        },
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
                "remove volume 'my-volume' for app 'my-app-uuid'",
                "setup volume 'my-volume' for app 'my-app-uuid'",
            ),
        ),
    );
}

#[test]
fn it_finds_a_workflow_to_create_multiple_volumes_and_finalizes_release_after_volume_create() {
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
                            "volumes": {
                                "vol-a": {
                                    "volume_name": "my-app-uuid_vol-a",
                                },
                                "vol-b": {
                                    "volume_name": "my-app-uuid_vol-b",
                                },
                            },
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "setup volume 'vol-a' for app 'my-app-uuid'",
                "setup volume 'vol-b' for app 'my-app-uuid'",
            )
            + seq!("finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",),
    );
}
