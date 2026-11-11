use super::helpers::*;

use mahler::dag::{Dag, dag, par, seq};
use serde_json::{Value, json};

const OLD_IMG_1: &str = "registry2.balena-cloud.com/v2/oldsvc1@sha256:a111111111111111111111111111111111111111111111111111111111111111";
const NEW_IMG_1: &str = "registry2.balena-cloud.com/v2/newsvc1@sha256:b111111111111111111111111111111111111111111111111111111111111111";
const OLD_IMG_2: &str = "registry2.balena-cloud.com/v2/oldsvc2@sha256:a222222222222222222222222222222222222222222222222222222222222222";
const NEW_IMG_2: &str = "registry2.balena-cloud.com/v2/newsvc2@sha256:b222222222222222222222222222222222222222222222222222222222222222";

fn labels(strategy: &str) -> Value {
    json!({ "io.balena.update.strategy": strategy })
}

/// A service of the old release with a container in the given state
fn old_service(id: u32, image: &str, strategy: &str, oci: Value) -> Value {
    json!({
        "id": id,
        "image": image,
        "started": true,
        "oci": oci,
        "config": { "labels": labels(strategy) },
    })
}

fn new_service(id: u32, image: &str, strategy: &str) -> Value {
    json!({
        "id": id,
        "image": image,
        "started": true,
        "config": { "labels": labels(strategy) },
    })
}

fn image(oci_id: &str) -> Value {
    json!({ "download_progress": 100, "oci_id": oci_id })
}

fn device(services: Value, images: Value) -> Value {
    json!({
        "uuid": "my-device-uuid",
        "apps": {
            "my-app-uuid": {
                "id": 1,
                "name": "my-app",
                "releases": {
                    "old-release": { "installed": true, "services": services }
                }
            }
        },
        "images": images,
    })
}

fn target(services: Value) -> Value {
    json!({
        "uuid": "my-device-uuid",
        "apps": {
            "my-app-uuid": {
                "id": 1,
                "name": "my-app",
                "releases": {
                    "new-release": { "installed": true, "services": services }
                }
            }
        },
    })
}

const INIT_RELEASE: &str = "initialize release 'new-release' for app with uuid 'my-app-uuid'";
const TAKE_LOCKS: &str = "take locks for app with uuid 'my-app-uuid'";
const FINISH_RELEASE: &str = "finish release 'new-release' for app with uuid 'my-app-uuid'";
const REMOVE_OLD_RELEASE: &str = "remove release 'old-release' for app with uuid 'my-app-uuid'";
const RELEASE_LOCKS: &str = "release locks for app with uuid 'my-app-uuid'";

// mahler's DAG macros take `&'static str` steps
fn pull(img: &str) -> &'static str {
    format!("pull image '{img}'").leak()
}

fn delete(img: &str) -> &'static str {
    format!("delete image '{img}'").leak()
}

#[test]
fn it_kills_the_old_service_before_downloading_with_kill_then_download() {
    init_tracing();
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "kill-then-download", running_container("old-release_service1")),
            }),
            json!({ OLD_IMG_1: image("111") }),
        ),
        target(json!({
            "service1": new_service(1, NEW_IMG_1, "kill-then-download"),
        })),
        seq!(
            INIT_RELEASE,
            "initialize service 'service1' for release 'new-release'",
            TAKE_LOCKS,
            "stop service 'service1' for release 'old-release'",
            "uninstall service 'service1' for release 'old-release'",
            pull(NEW_IMG_1),
        ) + par!(
            REMOVE_OLD_RELEASE,
            "install service 'service1' for release 'new-release'"
        ) + seq!(
            "start service 'service1' for release 'new-release'",
            FINISH_RELEASE,
            RELEASE_LOCKS,
        ),
    );
}

#[test]
fn it_deletes_the_old_image_before_downloading_with_delete_then_download() {
    init_tracing();
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "delete-then-download", running_container("old-release_service1")),
            }),
            json!({ OLD_IMG_1: image("111") }),
        ),
        target(json!({
            "service1": new_service(1, NEW_IMG_1, "delete-then-download"),
        })),
        seq!(
            INIT_RELEASE,
            "initialize service 'service1' for release 'new-release'",
            TAKE_LOCKS,
            "stop service 'service1' for release 'old-release'",
            "uninstall service 'service1' for release 'old-release'",
            delete(OLD_IMG_1),
            pull(NEW_IMG_1),
        ) + par!(
            REMOVE_OLD_RELEASE,
            "install service 'service1' for release 'new-release'"
        ) + seq!(
            "start service 'service1' for release 'new-release'",
            FINISH_RELEASE,
            RELEASE_LOCKS,
        ),
    );
}

#[test]
fn it_keeps_an_image_another_service_uses_with_delete_then_download() {
    init_tracing();
    // service2 keeps running the old image of service1, so it is not deleted
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "delete-then-download", running_container("old-release_service1")),
                "service2": old_service(2, OLD_IMG_1, "download-then-kill", running_container("old-release_service2")),
            }),
            json!({ OLD_IMG_1: image("111") }),
        ),
        target(json!({
            "service1": new_service(1, NEW_IMG_1, "delete-then-download"),
            "service2": new_service(2, OLD_IMG_1, "download-then-kill"),
        })),
        seq!(INIT_RELEASE)
            + par!(
                "initialize service 'service1' for release 'new-release'",
                "initialize service 'service2' for release 'new-release'",
            )
            + seq!(
                TAKE_LOCKS,
                "stop service 'service1' for release 'old-release'",
                "uninstall service 'service1' for release 'old-release'",
                pull(NEW_IMG_1),
            )
            + dag!(
                seq!("install service 'service1' for release 'new-release'"),
                par!(
                    "remove data for 'service2' for release 'old-release'",
                    "migrate service 'service2' to release 'new-release'",
                ),
            )
            + par!(
                REMOVE_OLD_RELEASE,
                "start service 'service1' for release 'new-release'"
            )
            + seq!(FINISH_RELEASE, RELEASE_LOCKS),
    );
}

#[test]
fn it_deletes_the_image_of_a_removed_service_with_delete_then_download() {
    init_tracing();
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "delete-then-download", running_container("old-release_service1")),
                "service2": old_service(2, OLD_IMG_2, "download-then-kill", running_container("old-release_service2")),
            }),
            json!({ OLD_IMG_1: image("111"), OLD_IMG_2: image("222") }),
        ),
        target(json!({
            "service2": new_service(2, NEW_IMG_2, "download-then-kill"),
        })),
        seq!(
            INIT_RELEASE,
            "initialize service 'service2' for release 'new-release'",
            pull(NEW_IMG_2),
            "install service 'service2' for release 'new-release'",
            TAKE_LOCKS,
            "stop service 'service1' for release 'old-release'",
            "uninstall service 'service1' for release 'old-release'",
            delete(OLD_IMG_1),
            "stop service 'service2' for release 'old-release'",
            "uninstall service 'service2' for release 'old-release'",
        ) + par!(
            REMOVE_OLD_RELEASE,
            "start service 'service2' for release 'new-release'"
        ) + seq!(FINISH_RELEASE, RELEASE_LOCKS),
    );
}

#[test]
fn it_only_holds_back_the_download_of_the_service_being_killed() {
    init_tracing();
    // service2 is download-then-kill, so its image is pulled right away and it
    // is only stopped once every image is ready
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "kill-then-download", running_container("old-release_service1")),
                "service2": old_service(2, OLD_IMG_2, "download-then-kill", running_container("old-release_service2")),
            }),
            json!({ OLD_IMG_1: image("111"), OLD_IMG_2: image("222") }),
        ),
        target(json!({
            "service1": new_service(1, NEW_IMG_1, "kill-then-download"),
            "service2": new_service(2, NEW_IMG_2, "download-then-kill"),
        })),
        seq!(INIT_RELEASE)
            + par!(
                "initialize service 'service1' for release 'new-release'",
                "initialize service 'service2' for release 'new-release'",
            )
            + seq!(
                pull(NEW_IMG_2),
                "install service 'service2' for release 'new-release'",
                TAKE_LOCKS,
                "stop service 'service1' for release 'old-release'",
                "uninstall service 'service1' for release 'old-release'",
                pull(NEW_IMG_1),
            )
            + dag!(
                seq!("install service 'service1' for release 'new-release'"),
                seq!(
                    "stop service 'service2' for release 'old-release'",
                    "uninstall service 'service2' for release 'old-release'",
                ),
            )
            + par!(
                REMOVE_OLD_RELEASE,
                "start service 'service1' for release 'new-release'",
                "start service 'service2' for release 'new-release'",
            )
            + seq!(FINISH_RELEASE, RELEASE_LOCKS),
    );
}

#[test]
fn it_migrates_an_unchanged_service_with_kill_then_download() {
    init_tracing();
    let unchanged = old_service(
        1,
        OLD_IMG_1,
        "kill-then-download",
        running_container("old-release_service1"),
    );
    assert_workflow(
        device(
            json!({ "service1": unchanged }),
            json!({ OLD_IMG_1: image("111") }),
        ),
        target(json!({
            "service1": new_service(1, OLD_IMG_1, "kill-then-download"),
        })),
        seq!(
            INIT_RELEASE,
            "initialize service 'service1' for release 'new-release'"
        ) + par!(
            "remove data for 'service1' for release 'old-release'",
            "migrate service 'service1' to release 'new-release'",
        ) + par!(FINISH_RELEASE, REMOVE_OLD_RELEASE),
    );
}

#[test]
fn it_removes_a_stopped_service_before_downloading_with_kill_then_download() {
    init_tracing();
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "kill-then-download", stopped_container("old-release_service1")),
            }),
            json!({ OLD_IMG_1: image("111") }),
        ),
        target(json!({
            "service1": new_service(1, NEW_IMG_1, "kill-then-download"),
        })),
        par!(
            INIT_RELEASE,
            "uninstall service 'service1' for release 'old-release'"
        ) + par!(
            REMOVE_OLD_RELEASE,
            "initialize service 'service1' for release 'new-release'"
        ) + seq!(
            pull(NEW_IMG_1),
            "install service 'service1' for release 'new-release'",
            "start service 'service1' for release 'new-release'",
            FINISH_RELEASE,
        ),
    );
}

#[test]
fn it_waits_for_pulls_to_remove_a_service_with_kill_then_download() {
    init_tracing();
    // no pull waits on the removed service, so it goes once every image is ready
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "kill-then-download", running_container("old-release_service1")),
                "service2": old_service(2, OLD_IMG_2, "download-then-kill", running_container("old-release_service2")),
            }),
            json!({ OLD_IMG_1: image("111"), OLD_IMG_2: image("222") }),
        ),
        target(json!({
            "service2": new_service(2, NEW_IMG_2, "download-then-kill"),
        })),
        seq!(
            INIT_RELEASE,
            "initialize service 'service2' for release 'new-release'",
            pull(NEW_IMG_2),
            "install service 'service2' for release 'new-release'",
            TAKE_LOCKS,
            "stop service 'service1' for release 'old-release'",
            "uninstall service 'service1' for release 'old-release'",
            "stop service 'service2' for release 'old-release'",
            "uninstall service 'service2' for release 'old-release'",
        ) + par!(
            REMOVE_OLD_RELEASE,
            "start service 'service2' for release 'new-release'"
        ) + seq!(FINISH_RELEASE, RELEASE_LOCKS),
    );
}

#[test]
fn it_waits_for_pulls_to_recreate_a_same_image_service_with_kill_then_download() {
    init_tracing();
    // service1 keeps its image, so killing it early would only lengthen its downtime
    let mut changed = new_service(1, OLD_IMG_1, "kill-then-download");
    changed["config"]["environment"] = json!({ "FOO": "bar" });
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "kill-then-download", running_container("old-release_service1")),
                "service2": old_service(2, OLD_IMG_2, "download-then-kill", running_container("old-release_service2")),
            }),
            json!({ OLD_IMG_1: image("111"), OLD_IMG_2: image("222") }),
        ),
        target(json!({
            "service1": changed,
            "service2": new_service(2, NEW_IMG_2, "download-then-kill"),
        })),
        seq!(INIT_RELEASE)
            + par!(
                "initialize service 'service1' for release 'new-release'",
                "initialize service 'service2' for release 'new-release'",
            )
            + seq!(pull(NEW_IMG_2))
            + par!(
                "install service 'service1' for release 'new-release'",
                "install service 'service2' for release 'new-release'",
            )
            + seq!(
                TAKE_LOCKS,
                "stop service 'service1' for release 'old-release'",
                "uninstall service 'service1' for release 'old-release'",
            )
            + dag!(
                seq!("start service 'service1' for release 'new-release'"),
                seq!(
                    "stop service 'service2' for release 'old-release'",
                    "uninstall service 'service2' for release 'old-release'",
                ),
            )
            + par!(
                REMOVE_OLD_RELEASE,
                "start service 'service2' for release 'new-release'"
            )
            + seq!(FINISH_RELEASE, RELEASE_LOCKS),
    );
}

#[test]
fn it_follows_the_strategy_of_the_target_service() {
    init_tracing();
    assert_workflow(
        device(
            json!({
                "service1": old_service(1, OLD_IMG_1, "download-then-kill", running_container("old-release_service1")),
            }),
            json!({ OLD_IMG_1: image("111") }),
        ),
        target(json!({
            "service1": new_service(1, NEW_IMG_1, "kill-then-download"),
        })),
        seq!(
            INIT_RELEASE,
            "initialize service 'service1' for release 'new-release'",
            TAKE_LOCKS,
            "stop service 'service1' for release 'old-release'",
            "uninstall service 'service1' for release 'old-release'",
            pull(NEW_IMG_1),
        ) + par!(
            REMOVE_OLD_RELEASE,
            "install service 'service1' for release 'new-release'"
        ) + seq!(
            "start service 'service1' for release 'new-release'",
            FINISH_RELEASE,
            RELEASE_LOCKS,
        ),
    );
}

fn with_network(mut svc: Value) -> Value {
    svc["config"]["networks"] = json!({ "my-net": {} });
    svc
}

fn network(driver: &str) -> Value {
    json!({ "my-net": { "oci_name": "my-app-uuid_my-net", "config": { "driver": driver, "enable_ipv6": false } } })
}

fn network_target(driver: &str) -> Value {
    json!({ "my-net": { "config": { "driver": driver, "enable_ipv6": false } } })
}

#[test]
fn it_kills_before_downloading_when_the_network_changes_too() {
    init_tracing();
    let mut current = device(
        json!({
            "service1": with_network(old_service(1, OLD_IMG_1, "kill-then-download", running_container("old-release_service1"))),
        }),
        json!({ OLD_IMG_1: image("111") }),
    );
    current["apps"]["my-app-uuid"]["releases"]["old-release"]["networks"] = network("bridge");
    let mut tgt = target(json!({
        "service1": with_network(new_service(1, NEW_IMG_1, "kill-then-download")),
    }));
    tgt["apps"]["my-app-uuid"]["releases"]["new-release"]["networks"] = network_target("overlay");
    assert_workflow(
        current,
        tgt,
        seq!(
            INIT_RELEASE,
            "initialize service 'service1' for release 'new-release'",
            TAKE_LOCKS,
            "stop service 'service1' for release 'old-release'",
            "uninstall service 'service1' for release 'old-release'",
            pull(NEW_IMG_1),
            "remove network 'my-net' for app 'my-app-uuid'",
        ) + par!(
            REMOVE_OLD_RELEASE,
            "setup network 'my-net' for app 'my-app-uuid'"
        ) + seq!(
            "install service 'service1' for release 'new-release'",
            "start service 'service1' for release 'new-release'",
            FINISH_RELEASE,
            RELEASE_LOCKS,
        ),
    );
}
