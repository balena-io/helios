use super::helpers::*;

use mahler::dag::{Dag, par, seq};
use serde_json::json;

#[test]
fn it_finds_a_workflow_to_update_the_hostapp_on_a_fresh_device() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running",
                    }
                }
            },
        }),
        seq!(
            "initialize host OS release 'target-release'",
            "install host OS release 'target-release'",
            "reboot to activate host OS release 'target-release'",
        ),
    );
}

#[test]
fn it_finds_a_workflow_to_update_the_hostapp_to_a_new_release() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
                "releases": {
                    "old-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "abcd1234",
                            "install_attempts": 1,
                        },
                        "status": "running",
                    }
                }
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "new-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running"
                    }
                }
            },
        }),
        seq!("initialize host OS release 'new-release'",)
            + par!(
                "install host OS release 'new-release'",
                "remove metadata for host OS release 'old-release'",
            )
            + seq!("reboot to activate host OS release 'new-release'"),
    );
}

#[test]
fn it_skips_a_hostapp_install_if_already_installed() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "auths": [],
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
                "releases": {
                    "old-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "abcd1234",
                            "install_attempts": 1,
                        },
                        "status": "running",
                    },
                    "new-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 1,
                        },
                        "status": "installed",
                    }
                }
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "new-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running"
                    }
                }
            },
        }),
        par!(
            "reboot to activate host OS release 'new-release'",
            "remove metadata for host OS release 'old-release'",
        ),
    );
}

#[test]
fn it_skips_a_hostapp_install_after_too_many_install_failures() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "auths": [],
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
                "releases": {
                    "old-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "abcd1234",
                            "install_attempts": 1,
                        },
                        "status": "running",
                    },
                    "new-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 4,
                        },
                        "status": "created",
                    }
                }
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "new-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running"
                    }
                }
            },
        }),
        seq!("remove metadata for host OS release 'old-release'",),
    );
}

#[test]
fn it_ignores_a_target_that_deletes_the_hostapp() {
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234",
                },
                "releases": {
                    "old-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "abcd1234",
                            "install_attempts": 1,
                        },
                        "status": "running",
                    }
                }
            },
        }),
        json!({
            "name": "new-device-name",
            "uuid": "my-device-uuid",
        }),
        seq!("update device name",),
    );
}

#[test]
fn it_adopts_a_running_release_with_no_recorded_state() {
    // A device can reach the target build with nothing recorded for it, for
    // instance a fresh flash whose rootfs is already the target. It must adopt
    // the release rather than re-install the OS it is already running, and
    // with no overlays to activate there is nothing to reboot for either.
    init_tracing();
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "cde2354",
                },
                "releases": {}
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running"
                    }
                }
            },
        }),
        seq!("initialize host OS release 'target-release'"),
    );
}
