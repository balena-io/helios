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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "running",
                    }
                }
            },
        }),
        seq!(
            "initialize host OS release 'target-release'",
            "install host OS release 'target-release'",
        ),
    );
}

#[test]
fn it_deploys_and_activates_overlays_without_install_when_already_running_the_target() {
    // Fresh flash whose rootfs IS the target release (current meta.build ==
    // target build): the release needs no balenahup install, but its
    // reboot-requiring overlay must still be deployed and activated with a
    // single coordinated reboot.
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
            },
        }),
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "class": "overlay",
                                "requires_reboot": true,
                                "status": "active",
                            }
                        }
                    }
                }
            },
        }),
        seq!("initialize host OS release 'target-release'")
            + seq!("deploy overlay 'kernel-modules' for host OS release 'target-release'")
            + seq!("reboot to activate host OS release 'target-release'"),
    );
}

#[test]
fn it_deploys_overlays_before_installing_the_hostapp() {
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "class": "overlay",
                                "requires_reboot": true,
                                "status": "active",
                            }
                        }
                    }
                }
            },
        }),
        seq!("initialize host OS release 'target-release'")
            + seq!("deploy overlay 'kernel-modules' for host OS release 'target-release'")
            + seq!("install host OS release 'target-release'"),
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "abcd1234",
                        "status": "running",
                        "install_attempts": 1,
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "running"
                    }
                }
            },
        }),
        seq!("initialize host OS release 'new-release'",)
            + par!(
                "install host OS release 'new-release'",
                "remove metadata for host OS release 'old-release'",
            ),
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "abcd1234",
                        "status": "running",
                        "install_attempts": 1,
                    },
                    "new-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "installed",
                        "install_attempts": 1,
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "running"
                    }
                }
            },
        }),
        seq!("remove metadata for host OS release 'old-release'",),
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "abcd1234",
                        "status": "running",
                        "install_attempts": 1,
                    },
                    "new-release": {
                        "app": "hostapp-uuid",
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
                        "status": "created",
                        "install_attempts": 4,
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "cde2354",
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
                        "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                        "updater": "bh.cr/balena_os/balenahup",
                        "build": "abcd1234",
                        "status": "running",
                        "install_attempts": 1,
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
