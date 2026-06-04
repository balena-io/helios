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
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        seq!(
            "initialize host OS release 'target-release'",
            "deploy overlay 'kernel-modules' for host OS release 'target-release'",
            "reboot to activate host OS release 'target-release'",
        ),
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
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        seq!(
            "initialize host OS release 'target-release'",
            "deploy overlay 'kernel-modules' for host OS release 'target-release'",
            "install host OS release 'target-release'",
            "reboot to activate host OS release 'target-release'",
        ),
    );
}

#[test]
fn it_deploys_multiple_overlays_before_installing() {
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            },
                            "extra-modules": {
                                "image": "registry2.balena-cloud.com/v2/extramodules@sha256:c333333333333333333333333333333333333333333333333333333333333333",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        // init -> (deploy both overlays, in some order) -> install -> reboot.
        // Overlay map keys are sorted (Map derefs to BTreeMap).
        seq!("initialize host OS release 'target-release'")
            + par!(
                "deploy overlay 'extra-modules' for host OS release 'target-release'",
                "deploy overlay 'kernel-modules' for host OS release 'target-release'",
            )
            + seq!(
                "install host OS release 'target-release'",
                "reboot to activate host OS release 'target-release'",
            ),
    );
}

#[test]
fn it_deploys_a_missing_target_overlay_before_the_reboot() {
    // Already running the target OS with one overlay staged, the target adds a
    // second overlay. The reboot must wait for the new overlay to deploy so a
    // single coordinated reboot activates both, rather than rebooting on the
    // already-staged one and needing a second reboot for the new one.
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
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "deployed",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            }
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            },
                            "extra-modules": {
                                "image": "registry2.balena-cloud.com/v2/extramodules@sha256:c333333333333333333333333333333333333333333333333333333333333333",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            }
        }),
        seq!(
            "deploy overlay 'extra-modules' for host OS release 'target-release'",
            "reboot to activate host OS release 'target-release'",
        ),
    );
}

#[test]
fn it_removes_an_overlay_dropped_from_the_target() {
    // A live release keeps running while the target drops one of its overlays.
    // The overlay stays OS-compatible, so the stale-OS sweep would not reap it;
    // helios must plan the teardown itself. Removal needs no reboot and no
    // reinstall, so the plan is exactly the single remove step.
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
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
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
        seq!("remove overlay 'kernel-modules' for host OS release 'target-release'"),
    );
}

#[test]
fn it_aborts_when_an_overlay_activation_failed() {
    // A previous deploy ran the activation container and it exited non-zero, so
    // the read derives the overlay as failed and the container is still there.
    init_tracing();
    assert_aborted(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "cde2354",
                },
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "failed",
                                "runtime": "extension",
                            }
                        }
                    }
                }
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        "overlay activation failed for host OS release 'target-release', check device",
    );
}

#[test]
fn it_redeploys_an_overlay_whose_activation_failed_at_a_new_image() {
    // The terminal abort above is scoped to retrying the image that failed. A
    // new image is a different bet, so a release shipped to replace a broken
    // overlay still gets its attempt.
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
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "failed",
                                "runtime": "extension",
                            }
                        }
                    }
                }
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:c333333333333333333333333333333333333333333333333333333333333333",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        seq!(
            "remove overlay 'kernel-modules' for host OS release 'target-release'",
            "deploy overlay 'kernel-modules' for host OS release 'target-release'",
            "reboot to activate host OS release 'target-release'",
        ),
    );
}

#[test]
fn it_redeploys_an_overlay_whose_target_image_changed() {
    // Same release, new overlay image. The overlay key already exists, so the
    // deploy task cannot be selected on its own: the stale container has to go
    // first for the planner to converge on the new image.
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
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:c333333333333333333333333333333333333333333333333333333333333333",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        seq!(
            "remove overlay 'kernel-modules' for host OS release 'target-release'",
            "deploy overlay 'kernel-modules' for host OS release 'target-release'",
            "reboot to activate host OS release 'target-release'",
        ),
    );
}

#[test]
fn it_redeploys_an_overlay_whose_kernel_did_not_boot() {
    // Same release, same image, but the boot did not come up on the kernel this
    // overlay claims: its arming never took effect (a rollback restored another
    // override, or the device fell back to stock). The container exists and
    // matches the target image, so nothing in the image diff would notice. The
    // remedy is the deploy path, which re-runs the hooks and re-arms.
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
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "stale",
                                "runtime": "extension",
                            }
                        }
                    }
                }
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
        seq!(
            "remove overlay 'kernel-modules' for host OS release 'target-release'",
            "deploy overlay 'kernel-modules' for host OS release 'target-release'",
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
fn it_waits_while_the_os_release_is_being_validated() {
    init_tracing();
    // The in-progress exception defers the only divergent work (the install),
    // leaving the wait itself as the whole plan. It fails at run time, which is
    // what brings the apply back once the window closes.
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234"
                },
                "host_validating": true,
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "abcd1234",
                            "install_attempts": 1,
                        },
                        "status": "created"
                    }
                }
            }
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
            }
        }),
        seq!("wait for the host validation to finish"),
    );
}

#[test]
fn it_defers_installing_a_new_target_release_while_validation_runs() {
    init_tracing();
    // A release that first appears in the target during the validation window
    // must not be installed or rebooted (the guard is device-global). The wait
    // heads the plan, so the install and the reboot behind it are unreachable:
    // the wait fails and the workflow stops there. They are planned at all only
    // because the planner simulates the wait succeeding, which is what puts
    // them in the right order for the retry that follows.
    assert_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "abcd1234"
                },
                "host_validating": true,
                "releases": {}
            }
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
            }
        }),
        seq!(
            "wait for the host validation to finish",
            "initialize host OS release 'new-release'",
            "install host OS release 'new-release'",
            "reboot to activate host OS release 'new-release'"
        ),
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

#[test]
fn it_plans_nothing_when_the_release_and_its_overlays_are_converged() {
    // A converged release plans nothing, so no reboot is issued.
    init_tracing();
    assert_empty_workflow(
        json!({
            "name": "device-name",
            "uuid": "my-device-uuid",
            "host": {
                "meta": {
                    "name": "balenaOS",
                    "version": "5.7.3",
                    "build": "cde2354",
                },
                "releases": {
                    "target-release": {
                        "app": "hostapp-uuid",
                        "hostapp": {
                            "image": "registry2.balena-cloud.com/v2/hostapp@sha256:a111111111111111111111111111111111111111111111111111111111111111",
                            "updater": "bh.cr/balena_os/balenahup",
                            "build": "cde2354",
                            "install_attempts": 0,
                        },
                        "status": "running",
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
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
                        "overlays": {
                            "kernel-modules": {
                                "image": "registry2.balena-cloud.com/v2/kernelmodules@sha256:b222222222222222222222222222222222222222222222222222222222222222",
                                "status": "active",
                                "runtime": "extension",
                            }
                        }
                    }
                }
            },
        }),
    );
}
