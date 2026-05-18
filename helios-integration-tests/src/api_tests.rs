use bollard::Docker;
use bollard::config::RestartPolicy;
use bollard::config::RestartPolicyNameEnum;
use reqwest::StatusCode;
use serde_json::{Value, json};

use super::common::{
    HELIOS_URL, clear_reports, prune_images, wait_for_report, wait_for_target_apply,
};

const TEST_APP_UUID: &str = "test-app";

// --- Target state builders ---

/// Build the JSON body for a release with the given services, networks, and volumes.
/// `services` is a slice of `(name, json_value)` pairs where the json_value is the
/// full service object (id, image, composition). Add a `"networks"` key inside
/// `composition` to exercise service-level network configuration.
fn release_json(
    services: &[(&str, Value)],
    networks: &[(&str, Value)],
    volumes: &[(&str, Value)],
) -> Value {
    let services_obj: serde_json::Map<String, Value> = services
        .iter()
        .map(|(name, val)| (name.to_string(), val.clone()))
        .collect();
    let networks_obj: serde_json::Map<String, Value> = networks
        .iter()
        .map(|(name, val)| (name.to_string(), val.clone()))
        .collect();
    let volumes_obj: serde_json::Map<String, Value> = volumes
        .iter()
        .map(|(name, val)| (name.to_string(), val.clone()))
        .collect();
    json!({
        "services": services_obj,
        "networks": networks_obj,
        "volumes": volumes_obj,
    })
}

fn app_target_json(app_name: &str, release_uuid: &str, release: Value) -> Value {
    json!({
        "id": 0,
        "name": app_name,
        "releases": {
            release_uuid: release,
        }
    })
}

// --- State response helpers ---

fn get_service(app: &Value, release_uuid: &str, service_name: &str) -> Value {
    app.pointer(&format!("/releases/{release_uuid}/services/{service_name}"))
        .unwrap_or_else(|| panic!("service '{service_name}' not found in release '{release_uuid}'"))
        .clone()
}

fn get_service_container_id<'a>(app: &'a Value, release_uuid: &str, service_name: &str) -> &'a str {
    app.pointer(&format!(
        "/releases/{release_uuid}/services/{service_name}/oci/name"
    ))
    .unwrap_or_else(|| {
        panic!("container id not found for service '{service_name}' in release '{release_uuid}'")
    })
    .as_str()
    .unwrap()
}

/// Returns the `oci_name` for a release-level resource (network or volume).
/// `resource_kind` is `"networks"` or `"volumes"`.
fn get_resource_oci_name<'a>(
    app: &'a Value,
    release_uuid: &str,
    resource_kind: &str,
    resource_name: &str,
) -> &'a str {
    app.pointer(&format!(
        "/releases/{release_uuid}/{resource_kind}/{resource_name}/oci_name"
    ))
    .unwrap_or_else(|| {
        panic!("oci_name not found for {resource_kind}/{resource_name} in release '{release_uuid}'")
    })
    .as_str()
    .unwrap()
}

// --- Container assertion helpers ---

fn assert_env_contains(env: &[String], expected: &[&str]) {
    for entry in expected {
        assert!(
            env.contains(&entry.to_string()),
            "env missing '{entry}'; got {env:?}"
        );
    }
}

fn assert_container_networks(
    container: &bollard::models::ContainerInspectResponse,
    expected_oci_names: &[&str],
) {
    let nets = container
        .network_settings
        .as_ref()
        .and_then(|s| s.networks.as_ref())
        .expect("container has no network settings");
    let mut actual: Vec<String> = nets.keys().cloned().collect();
    actual.sort();
    let mut expected: Vec<String> = expected_oci_names.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(actual, expected, "container network connections mismatch");
}

fn assert_endpoint_aliases(
    container: &bollard::models::ContainerInspectResponse,
    network_oci_name: &str,
    expected_aliases: &[&str],
) {
    let endpoint = container
        .network_settings
        .as_ref()
        .and_then(|s| s.networks.as_ref())
        .and_then(|n| n.get(network_oci_name))
        .unwrap_or_else(|| panic!("container not connected to '{network_oci_name}'"));
    let aliases = endpoint.aliases.clone().unwrap_or_default();
    for expected in expected_aliases {
        assert!(
            aliases.iter().any(|a| a == expected),
            "endpoint '{network_oci_name}' missing alias '{expected}'; got {aliases:?}"
        );
    }
}

async fn assert_network_created(docker: &Docker, oci_name: &str, logical_name: &str) {
    let network = docker.inspect_network(oci_name, None).await.unwrap();
    assert_eq!(
        network
            .labels
            .unwrap_or_default()
            .get("io.balena.network-name")
            .unwrap(),
        logical_name
    );
}

/// Locate a mount entry on an inspected container by its `Destination` (target path).
fn container_mount<'a>(
    container: &'a bollard::models::ContainerInspectResponse,
    target: &str,
) -> &'a bollard::models::MountPoint {
    container
        .mounts
        .as_ref()
        .expect("container has no mounts")
        .iter()
        .find(|m| m.destination.as_deref() == Some(target))
        .unwrap_or_else(|| panic!("no mount at target '{target}'"))
}

// --- Teardown ---

async fn delete_app_and_wait(client: &reqwest::Client, app_uuid: &str) {
    let res = client
        .delete(format!("{HELIOS_URL}/v3/device/apps/{app_uuid}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));
}

// --- Tests ---

#[tokio::test]
async fn test_service_running() {
    let body = reqwest::get(format!("{HELIOS_URL}/ping"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "OK")
}

#[tokio::test]
async fn test_initial_state() {
    let body: serde_json::Value = reqwest::get(format!("{HELIOS_URL}/v3/device"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body.get("uuid"), Some(&json!("test-uuid")));
    assert_eq!(body.get("apps"), Some(&json!({})));
    assert_eq!(body.get("images"), Some(&json!({})));
}

#[tokio::test]
async fn test_get_app_state() {
    let client = reqwest::Client::new();
    let body = client
        .get(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_set_app_target() {
    let client = reqwest::Client::new();
    let target = json!({"id": 0, "name": "my-app"});
    let body = client
        .post(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .json(&target)
        .send()
        .await
        .unwrap();
    assert_eq!(body.status(), StatusCode::ACCEPTED);
    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));

    let app: serde_json::Value =
        reqwest::get(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

    assert_eq!(
        app,
        json!({"id": 0, "name": "my-app", "locked": false, "lockfiles": [], "releases": {}})
    );

    delete_app_and_wait(&client, TEST_APP_UUID).await;
}

#[tokio::test]
async fn test_set_app_target_install_images() {
    prune_images().await;
    clear_reports().await;

    let release_uuid = "my-release-uuid";

    let release = release_json(
        &[
            (
                "service-one",
                json!({
                    "id": 1,
                    "image": "ubuntu:latest",
                    "composition": {
                        "command": "sleep infinity",
                        "restart": "no",
                        "networks": {
                            "net-a": { "aliases": ["svc-one-alias"] },
                            "net-b": null,
                        },
                        "volumes": [
                            "my-vol:/backup:ro",
                        ],
                    }
                }),
            ),
            (
                "service-two",
                json!({
                    "id": 2,
                    "image": "alpine:latest",
                    "composition": {
                        "command": ["sleep", "10"],
                        "labels": { "my-label": "true" },
                        "environment": ["MY_KEY=123"],
                        "volumes": [
                            {
                                "type": "volume",
                                "source": "my-vol",
                                "target": "/data",
                                "read_only": true
                            },
                            {
                                "type": "bind",
                                "source": "/proc",
                                "target": "/host/proc"
                            },
                            {
                                "type": "tmpfs",
                                "target": "/run/tmp",
                                "tmpfs": { "size": 65536, "mode": 448 }
                            }
                        ],
                    }
                }),
            ),
            (
                "service-three",
                json!({
                    "id": 3,
                    "image": "alpine:latest",
                    "composition": {
                        "command": ["sleep", "infinity"],
                        "network_mode": "host",
                    }
                }),
            ),
        ],
        &[("net-a", json!({})), ("net-b", json!({}))],
        &[("my-vol", json!({}))],
    );

    let target = app_target_json("my-new-app-name", release_uuid, release);

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .json(&target)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let status = wait_for_target_apply().await;
    assert_eq!(status, json!({"status": "done"}));

    // Fetch current device state
    let device: Value = reqwest::get(format!("{HELIOS_URL}/v3/device"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let app = device
        .get("apps")
        .and_then(|apps| apps.get(TEST_APP_UUID))
        .unwrap();

    // Images were pulled and reported
    let images = device.get("images").unwrap();
    let ubuntu_img_id = images.get("ubuntu:latest").unwrap().get("oci_id").unwrap();
    let alpine_img_id = images.get("alpine:latest").unwrap().get("oci_id").unwrap();

    let docker = Docker::connect_with_defaults().unwrap();
    assert_eq!(
        docker
            .inspect_image("ubuntu:latest")
            .await
            .unwrap()
            .id
            .unwrap(),
        *ubuntu_img_id
    );
    assert_eq!(
        docker
            .inspect_image("alpine:latest")
            .await
            .unwrap()
            .id
            .unwrap(),
        *alpine_img_id
    );

    // service-two environment variable surfaced in reported state
    let svc_two = get_service(app, release_uuid, "service-two");
    let my_env = svc_two
        .get("config")
        .and_then(|c| c.get("environment"))
        .and_then(|e| e.get("MY_KEY"))
        .unwrap();
    assert_eq!(my_env, &json!(123));

    // Containers exist and have the right configuration
    let svc_one_container_id = get_service_container_id(app, release_uuid, "service-one");
    let container = docker
        .inspect_container(svc_one_container_id, None)
        .await
        .unwrap();
    let config = container.config.unwrap();
    let host_config = container.host_config.unwrap();
    assert_eq!(config.cmd.unwrap(), vec!["sleep", "infinity"]);
    assert_eq!(
        host_config.restart_policy.unwrap(),
        RestartPolicy {
            name: Some(RestartPolicyNameEnum::NO),
            maximum_retry_count: Some(0),
        }
    );
    assert_eq!(
        config.labels.unwrap().get("io.balena.app-uuid").unwrap(),
        TEST_APP_UUID
    );
    assert_env_contains(
        &config.env.unwrap(),
        &[
            &format!("BALENA_APP_UUID={TEST_APP_UUID}"),
            "BALENA_DEVICE_UUID=test-uuid",
            "BALENA_HOST_OS_VERSION=balenaOS 6.0.39",
            "BALENA_HOST_OS_BUILD=test-board-rev-000",
        ],
    );

    let svc_two_container_id = get_service_container_id(app, release_uuid, "service-two");
    let container = docker
        .inspect_container(svc_two_container_id, None)
        .await
        .unwrap();
    let config = container.config.unwrap();
    let host_config = container.host_config.unwrap();
    assert_eq!(config.cmd.unwrap(), vec!["sleep", "10"]);
    assert_eq!(
        host_config.restart_policy.unwrap(),
        RestartPolicy {
            name: Some(RestartPolicyNameEnum::ALWAYS),
            maximum_retry_count: Some(0),
        }
    );
    let labels = config.labels.unwrap();
    assert_eq!(labels.get("io.balena.app-uuid").unwrap(), TEST_APP_UUID);
    assert_eq!(labels.get("my-label").unwrap(), "true");
    assert_env_contains(&config.env.unwrap(), &["MY_KEY=123"]);

    // Three networks exist: two defined in the release plus the auto-injected `default`
    // for services without an explicit network configuration.
    let net_a_id = get_resource_oci_name(app, release_uuid, "networks", "net-a").to_string();
    let net_b_id = get_resource_oci_name(app, release_uuid, "networks", "net-b").to_string();
    let default_net_id =
        get_resource_oci_name(app, release_uuid, "networks", "default").to_string();
    assert_network_created(&docker, &net_a_id, "net-a").await;
    assert_network_created(&docker, &net_b_id, "net-b").await;
    assert_network_created(&docker, &default_net_id, "default").await;

    // service-one is connected to both explicitly declared networks, not to `default`.
    let svc_one_container = docker
        .inspect_container(svc_one_container_id, None)
        .await
        .unwrap();
    assert_container_networks(&svc_one_container, &[&net_a_id, &net_b_id]);
    // The net-a endpoint carries the user-declared alias plus the service name,
    // which the state layer appends so the container is resolvable by its service name.
    assert_endpoint_aliases(
        &svc_one_container,
        &net_a_id,
        &["svc-one-alias", "service-one"],
    );

    // service-two declared no networks, so it is attached only to the auto-injected `default`.
    let svc_two_container = docker
        .inspect_container(svc_two_container_id, None)
        .await
        .unwrap();
    assert_container_networks(&svc_two_container, &[&default_net_id]);

    // service-three uses network_mode: host, so it is attached to the `host` network
    // and not to the auto-injected `default`.
    let svc_three_container_id = get_service_container_id(app, release_uuid, "service-three");
    let svc_three_container = docker
        .inspect_container(svc_three_container_id, None)
        .await
        .unwrap();
    assert_eq!(
        svc_three_container
            .host_config
            .as_ref()
            .and_then(|hc| hc.network_mode.as_deref()),
        Some("host")
    );
    assert_container_networks(&svc_three_container, &["host"]);

    let my_vol_id = get_resource_oci_name(app, release_uuid, "volumes", "my-vol").to_string();
    let volume = docker.inspect_volume(&my_vol_id).await.unwrap();
    assert_eq!(
        volume.labels.get("io.balena.volume-name").unwrap(),
        "my-vol"
    );

    // service-one received the short-form volume mount `my-vol:/backup:ro`.
    // Docker reports the mount source as the namespaced volume name.
    let svc_one_backup = container_mount(&svc_one_container, "/backup");
    assert_eq!(svc_one_backup.typ.as_ref().unwrap(), "volume");
    assert_eq!(svc_one_backup.name.as_deref(), Some(my_vol_id.as_str()));
    assert_eq!(svc_one_backup.rw, Some(false));

    // service-two declares a volume mount, a bind mount, and a tmpfs mount
    // via the long-form syntax.
    let svc_two_data = container_mount(&svc_two_container, "/data");
    assert_eq!(svc_two_data.typ.as_ref().unwrap(), "volume");
    assert_eq!(svc_two_data.name.as_deref(), Some(my_vol_id.as_str()));
    assert_eq!(svc_two_data.rw, Some(false));

    let svc_two_machine_id = container_mount(&svc_two_container, "/host/proc");
    assert_eq!(svc_two_machine_id.typ.as_ref().unwrap(), "bind");
    assert_eq!(svc_two_machine_id.source.as_deref(), Some("/proc"));
    assert_eq!(svc_two_machine_id.rw, Some(true));

    let svc_two_tmp = container_mount(&svc_two_container, "/run/tmp");
    assert_eq!(svc_two_tmp.typ.as_ref().unwrap(), "tmpfs");

    // The host-level tmpfs options that were sent to the engine should round-trip
    // through the `HostConfig.Mounts` spec.
    let svc_two_host_mounts = svc_two_container
        .host_config
        .as_ref()
        .and_then(|hc| hc.mounts.as_ref())
        .expect("host_config.mounts not set");
    let tmpfs_spec = svc_two_host_mounts
        .iter()
        .find(|m| m.target.as_deref() == Some("/run/tmp"))
        .expect("tmpfs mount not found in host_config.mounts");
    let tmpfs_opts = tmpfs_spec
        .tmpfs_options
        .as_ref()
        .expect("tmpfs options missing");
    assert_eq!(tmpfs_opts.size_bytes, Some(65536));
    assert_eq!(tmpfs_opts.mode, Some(448));

    // State report was sent with correct service statuses
    let release_report = wait_for_report(TEST_APP_UUID, release_uuid, "done", 10).await;
    assert_eq!(
        release_report["services"]["service-one"]["status"],
        "Running"
    );
    assert_eq!(
        release_report["services"]["service-two"]["status"],
        "Running"
    );
    assert_eq!(
        release_report["services"]["service-three"]["status"],
        "Running"
    );

    // Teardown: remove app and confirm images are cleaned up
    clear_reports().await;
    delete_app_and_wait(&client, TEST_APP_UUID).await;
    assert!(docker.inspect_image("ubuntu:latest").await.is_err());
    assert!(docker.inspect_image("alpine:latest").await.is_err());
}

#[tokio::test]
async fn test_set_app_target_rejects_bind_mount_not_in_allowlist() {
    let release_uuid = "reject-release";
    let release = release_json(
        &[(
            "bad-svc",
            json!({
                "id": 1,
                "image": "alpine:latest",
                "composition": {
                    "volumes": [
                        {"type": "bind", "source": "/root", "target": "/root"}
                    ]
                }
            }),
        )],
        &[],
        &[],
    );
    let target = app_target_json("reject-app", release_uuid, release);

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .json(&target)
        .send()
        .await
        .unwrap();

    // Axum returns a 4xx when the JSON body fails to deserialize; the
    // remote-model allowlist check fires during deserialization.
    assert!(
        res.status().is_client_error(),
        "expected 4xx, got {}",
        res.status()
    );

    // Confirm no app was created.
    let res = client
        .get(format!("{HELIOS_URL}/v3/device/apps/{TEST_APP_UUID}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
