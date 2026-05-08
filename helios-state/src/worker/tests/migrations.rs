use super::helpers::*;

use mahler::dag::{Dag, dag, par, seq};
use serde_json::json;

#[test]
fn it_finds_a_workflow_for_migrating_networks() {
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
                                "my-net": {
                                    "oci_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                        }
                    }
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
                        "new-release": {
                            "installed": true,
                            "services": {},
                            "networks": {
                                "my-net": {
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        seq!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "setup network 'my-net' for app 'my-app-uuid'",
        ) + par!(
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "remove data for network 'my-net' from release 'old-release'",
        ) + seq!("remove release 'old-release' for app with uuid 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_for_migrating_volumes() {
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
                            "volumes": {
                                "my-vol": {
                                    "oci_name": "my-app-uuid_my-vol",
                                    "config": {
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
                            "volumes": {
                                "my-vol": {
                                    "config": {
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
        seq!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "setup volume 'my-vol' for app 'my-app-uuid'",
        ) + par!(
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "remove data for volume 'my-vol' from release 'old-release'",
        ) + seq!("remove release 'old-release' for app with uuid 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_for_migrating_volumes_with_linked_services() {
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "oci": running_container("old-release_my-svc"),
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "oci_name": "my-app-uuid_my-vol",
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "111"
                },
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {},
                            },
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'",)
            + par!(
                "initialize service 'my-svc' for release 'new-release'",
                "setup volume 'my-vol' for app 'my-app-uuid'"
            )
            + seq!(
                "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'"
            )
            + dag!(
                par!(
                    "remove data for 'my-svc' for release 'old-release'",
                    "migrate service 'my-svc' to release 'new-release'"
                ),
                seq!("remove data for volume 'my-vol' from release 'old-release'"),
            )
            + par!(
                "remove release 'old-release' for app with uuid 'my-app-uuid'",
                "update image metadata for service 'my-svc' of release 'new-release'"
            )
            + seq!("finish release 'new-release' for app with uuid 'my-app-uuid'"),
    );
}

#[test]
fn it_should_not_migrate_a_service_that_links_to_a_changing_volume() {
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "oci": running_container("old-release_my-svc"),
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "oci_name": "my-app-uuid_my-vol",
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "111"
                },
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "config": {
                                        "labels": {
                                            "io.balena.foo": "bar"
                                        }
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        dag!(
            seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'"),
            seq!(
                "stop service 'my-svc' for release 'old-release'",
                "uninstall service 'my-svc' for release 'old-release'"
            )
        ) + par!(
            "initialize service 'my-svc' for release 'new-release'",
            "remove volume 'my-vol' for app 'my-app-uuid'",
        ) + seq!(
            "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'"
        ) + par!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "setup volume 'my-vol' for app 'my-app-uuid'"
        ) + seq!(
            "install service 'my-svc' for release 'new-release'",
            "start service 'my-svc' for release 'new-release'",
            "finish release 'new-release' for app with uuid 'my-app-uuid'"
        ),
    );
}

#[test]
fn it_should_not_migrate_a_service_that_links_to_a_changing_volume_and_network() {
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "oci": running_container("old-release_my-svc"),
                                    "config": {
                                        "networks": {"my-net": {}},
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "oci_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "oci_name": "my-app-uuid_my-vol",
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "111"
                },
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {
                                        "networks": {"my-net": {}},
                                        "volumes": [
                                            {
                                                "type": "volume",
                                                "source": "my-vol",
                                                "target": "/data"
                                            }
                                        ]
                                    },
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "config": {
                                        "driver_opts": {
                                            "foo": "baz"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "config": {
                                        "labels": {
                                            "io.balena.foo": "bar"
                                        }
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        dag!(
            seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'"),
            seq!(
                "stop service 'my-svc' for release 'old-release'",
                "uninstall service 'my-svc' for release 'old-release'"
            )
        ) + par!(
            "initialize service 'my-svc' for release 'new-release'",
            "remove network 'my-net' for app 'my-app-uuid'",
            "remove volume 'my-vol' for app 'my-app-uuid'",
        ) + seq!(
            "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'"
        ) + par!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "setup network 'my-net' for app 'my-app-uuid'",
            "setup volume 'my-vol' for app 'my-app-uuid'"
        ) + seq!(
            "install service 'my-svc' for release 'new-release'",
            "start service 'my-svc' for release 'new-release'",
            "finish release 'new-release' for app with uuid 'my-app-uuid'"
        ),
    );
}

#[test]
fn it_finds_a_workflow_for_migrating_networks_with_linked_services() {
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "oci": running_container("old-release_my-svc"),
                                    "config": {
                                        "networks": {"my-net": {}}
                                    },
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "oci_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "111"
                },
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {
                                        "networks": {"my-net": {}}
                                    },
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'",)
            + par!(
                "setup network 'my-net' for app 'my-app-uuid'",
                "initialize service 'my-svc' for release 'new-release'"
            )
            + seq!(
                "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'"
            )
            + dag!(
                seq!("remove data for network 'my-net' from release 'old-release'"),
                par!(
                    "remove data for 'my-svc' for release 'old-release'",
                    "migrate service 'my-svc' to release 'new-release'"
                )
            )
            + par!(
                "remove release 'old-release' for app with uuid 'my-app-uuid'",
                "update image metadata for service 'my-svc' of release 'new-release'"
            )
            + seq!("finish release 'new-release' for app with uuid 'my-app-uuid'"),
    );
}

#[test]
fn it_should_not_migrate_a_service_that_links_to_a_changing_network() {
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "oci": running_container("old-release_my-svc"),
                                    "config": {
                                        "networks": {"my-net": {}}
                                    },
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "oci_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "111"
                },
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {
                                        "networks": {"my-net": {}}
                                    },
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "config": {
                                        "driver_opts": {
                                            "foo": "baz"
                                        },
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        dag!(
            seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'"),
            seq!(
                "stop service 'my-svc' for release 'old-release'",
                "uninstall service 'my-svc' for release 'old-release'"
            )
        ) + par!(
            "initialize service 'my-svc' for release 'new-release'",
            "remove network 'my-net' for app 'my-app-uuid'",
        ) + seq!(
            "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'"
        ) + par!(
            "remove release 'old-release' for app with uuid 'my-app-uuid'",
            "setup network 'my-net' for app 'my-app-uuid'"
        ) + seq!(
            "install service 'my-svc' for release 'new-release'",
            "start service 'my-svc' for release 'new-release'",
            "finish release 'new-release' for app with uuid 'my-app-uuid'"
        ),
    );
}

#[test]
fn it_finds_a_workflow_for_migrating_networks_and_volumes() {
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
                                "my-net": {
                                    "oci_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "oci_name": "my-app-uuid_my-vol",
                                    "config": {
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
                                "my-net": {
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "config": {
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
        seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'")
            + par!(
                "setup network 'my-net' for app 'my-app-uuid'",
                "setup volume 'my-vol' for app 'my-app-uuid'",
            )
            + par!(
                "finish release 'new-release' for app with uuid 'my-app-uuid'",
                "remove data for network 'my-net' from release 'old-release'",
                "remove data for volume 'my-vol' from release 'old-release'",
            )
            + seq!("remove release 'old-release' for app with uuid 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_for_migrating_services_networks_and_volumes() {
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as target, different URI name
                                    "image": "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "oci": running_container("old-release_my-svc"),
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "oci_name": "my-app-uuid_my-net",
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "oci_name": "my-app-uuid_my-vol",
                                    "config": {
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
            "images": {
                "registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111": {
                    "config": {},
                    "download_progress": 100,
                    "oci_id": "111"
                },
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
                            "services": {
                                "my-svc": {
                                    "id": 1,
                                    // same digest as old release, different URI name
                                    "image": "registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                                    "started": true,
                                    "config": {},
                                },
                            },
                            "networks": {
                                "my-net": {
                                    "config": {
                                        "driver_opts": {
                                            "foo": "bar"
                                        },
                                    },
                                },
                            },
                            "volumes": {
                                "my-vol": {
                                    "config": {
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
        seq!("initialize release 'new-release' for app with uuid 'my-app-uuid'")
            + par!(
                "setup network 'my-net' for app 'my-app-uuid'",
                "initialize service 'my-svc' for release 'new-release'",
                "setup volume 'my-vol' for app 'my-app-uuid'",
            )
            + seq!(
                "tag image 'registry2.balena-cloud.com/v2/oldsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111' as 'registry2.balena-cloud.com/v2/newsvc@sha256:a111111111111111111111111111111111111111111111111111111111111111'",
            )
            + dag!(
                seq!("remove data for network 'my-net' from release 'old-release'"),
                par!(
                    "remove data for 'my-svc' for release 'old-release'",
                    "migrate service 'my-svc' to release 'new-release'"
                ),
                seq!("remove data for volume 'my-vol' from release 'old-release'"),
            )
            + par!(
                "remove release 'old-release' for app with uuid 'my-app-uuid'",
                "update image metadata for service 'my-svc' of release 'new-release'",
            )
            + seq!("finish release 'new-release' for app with uuid 'my-app-uuid'",),
    );
}

#[test]
fn it_finds_a_workflow_for_migrating_and_recreating_networks_and_volumes() {
    init_tracing();

    // Old release has same-config network + volume (will migrate)
    // and a different-config network + volume (will uninstall and recreate)
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
                                "same-net": {
                                    "oci_name": "my-app-uuid_same-net",
                                    "config": {
                                        "driver_opts": { "foo": "bar" },
                                    },
                                },
                                "changed-net": {
                                    "oci_name": "my-app-uuid_changed-net",
                                    "config": {
                                        "enable_ipv6": false,
                                    },
                                },
                            },
                            "volumes": {
                                "same-vol": {
                                    "oci_name": "my-app-uuid_same-vol",
                                    "config": {
                                        "driver_opts": { "type": "nfs" },
                                    },
                                },
                                "changed-vol": {
                                    "oci_name": "my-app-uuid_changed-vol",
                                    "config": {
                                        "driver_opts": { "type": "local" },
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
                                "same-net": {
                                    "config": {
                                        "driver_opts": { "foo": "bar" },
                                    },
                                },
                                "changed-net": {
                                    "config": {
                                        "enable_ipv6": true,
                                    },
                                },
                            },
                            "volumes": {
                                "same-vol": {
                                    "config": {
                                        "driver_opts": { "type": "nfs" },
                                    },
                                },
                                "changed-vol": {
                                    "config": {
                                        "driver_opts": { "type": "tmpfs" },
                                    },
                                },
                            },
                        }
                    }
                }
            },
        }),
        // changed-net and changed-vol get uninstalled (Docker delete) then recreated
        // same-net and same-vol get migrated (state-only removal from old release)
        par!(
            "initialize release 'new-release' for app with uuid 'my-app-uuid'",
            "remove network 'changed-net' for app 'my-app-uuid'",
            "remove volume 'changed-vol' for app 'my-app-uuid'",
        ) + par!(
            "setup network 'changed-net' for app 'my-app-uuid'",
            "setup network 'same-net' for app 'my-app-uuid'",
            "setup volume 'changed-vol' for app 'my-app-uuid'",
            "setup volume 'same-vol' for app 'my-app-uuid'",
        ) + par!(
            "finish release 'new-release' for app with uuid 'my-app-uuid'",
            "remove data for network 'same-net' from release 'old-release'",
            "remove data for volume 'same-vol' from release 'old-release'",
        ) + seq!("remove release 'old-release' for app with uuid 'my-app-uuid'",),
    );
}
