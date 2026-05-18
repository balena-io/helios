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
                                    "image": "ubuntu:latest",
                                    "config": {},
                                },
                            },
                            "volumes": {
                                "my-volume": {},
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
                                "volume-b": {}
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
                                    "oci_name": "my-app-uuid_my-volume",
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
fn it_finds_a_workflow_to_install_a_service_with_a_volume_mount() {
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
                                "my-service": {
                                    "id": 1,
                                    "started": true,
                                    "image": "ubuntu:latest",
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-volume",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-volume": {},
                            },
                        }
                    }
                }
            },
        }),
        // Service references `my-volume`, so `setup volume` must complete before
        // the service install.
        seq!("initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'")
            + par!(
                "initialize service 'my-service' for release 'my-release-uuid'",
                "setup volume 'my-volume' for app 'my-app-uuid'",
            )
            + seq!(
                "pull image 'ubuntu:latest'",
                "install service 'my-service' for release 'my-release-uuid'",
                "start service 'my-service' for release 'my-release-uuid'",
                "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            ),
    );
}

#[test]
fn it_finds_a_workflow_for_updating_a_volume_in_use_by_a_service() {
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
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-volume",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-volume": {
                                    "oci_name": "my-app-uuid_my-volume",
                                    "config": {
                                        "driver": "local",
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
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-volume",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-volume": {
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
                "take locks for app with uuid 'my-app-uuid'",
                "stop service 'my-service' for release 'my-release-uuid'",
                "uninstall service 'my-service' for release 'my-release-uuid'",
            ) + par!(
                "initialize service 'my-service' for release 'my-release-uuid'",
                "remove volume 'my-volume' for app 'my-app-uuid'",
            ) + seq!(
                "setup volume 'my-volume' for app 'my-app-uuid'",
                "install service 'my-service' for release 'my-release-uuid'",
                "start service 'my-service' for release 'my-release-uuid'",
            ),
        ) + seq!("release locks for app with uuid 'my-app-uuid'"),
    );
}

#[test]
fn it_finds_a_workflow_to_install_a_service_with_bind_and_tmpfs_mounts() {
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
                                "my-service": {
                                    "id": 1,
                                    "started": true,
                                    "image": "ubuntu:latest",
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "bind",
                                                "source": "/etc/machine-id",
                                                "target": "/etc/machine-id"
                                            },
                                            {
                                                "type": "tmpfs",
                                                "target": "/tmp"
                                            }
                                        ]
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        // No top-level volumes are referenced, so there is no setup-volume step.
        seq!(
            "initialize release 'my-release-uuid' for app with uuid 'my-app-uuid'",
            "initialize service 'my-service' for release 'my-release-uuid'",
            "pull image 'ubuntu:latest'",
            "install service 'my-service' for release 'my-release-uuid'",
            "start service 'my-service' for release 'my-release-uuid'",
            "finish release 'my-release-uuid' for app with uuid 'my-app-uuid'",
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
                                "vol-a": {},
                                "vol-b": {},
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
