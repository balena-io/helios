use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};
use serde_json as json;

use crate::common_types::Uuid;
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_ID, LABEL_SERVICE_NAME, LABEL_SUPERVISED};
use crate::oci::{self, LocalNamespace, Mount, Namespace};

const LABEL_CONFIG_MAP: &str = "io.balena.private.config.map";
const LABEL_CONFIG_ALIASES: &str = "io.balena.private.config.aliases";
pub(super) const LABEL_DEPENDS_ON: &str = "io.balena.private.depends-on";
const ENV_APP_UUID: &str = "BALENA_APP_UUID";
const ENV_SERVICE_NAME: &str = "BALENA_SERVICE_NAME";

/// Build a map of the object keys present in the container config, recursively.
///
/// This creates a map of every key present in the given [`oci::ContainerConfig`]. The map is built
/// based on the serialization shape of the input. Fields skipped during serialization are assumed
/// to not be used in the container creation.
///
/// The resulting map will have the following shape
///
/// ```json
/// {"foo": null, "bar": {}, "baz": {...}}
/// ```
/// this is read as
/// - `foo` is a leaf, the composition set the value and the key should be read back from the engine
/// - `bar` has sub-keys, but none were set on the composition, so retrieved sub-keys should be dropped
/// - `baz` has sub-keys, recurse over them to determine what was set by the composition
/// - any key absent from the map is skipped on read back
fn build_map(config: &oci::ContainerConfig) -> json::Value {
    // Map the keys in the `value`, recursively
    fn shape_of(value: &json::Value) -> json::Value {
        match value {
            json::Value::Object(map) => map
                .iter()
                .map(|(key, value)| (key.clone(), shape_of(value)))
                .collect::<json::Map<_, _>>()
                .into(),
            _ => json::Value::Null,
        }
    }

    // we assume the config is serializable, if it isn't nothing is going to work anyway
    let value = json::to_value(config).expect("container config is serializable");
    shape_of(&value)
}

/// Remove the keys of of the input config that are not part of the map
///
/// Anything the composition sets will round-trip untouched, while defaults filled by the engine or
/// inherited from the image will be dropped.
fn prune_untracked(config: oci::ContainerConfig, map: json::Value) -> oci::ContainerConfig {
    /// Remove the object keys of `value` that are not part of `shape`.
    fn prune_tree(value: &mut json::Value, shape: &json::Value) {
        let (json::Value::Object(value), json::Value::Object(shape)) = (value, shape) else {
            return;
        };
        value.retain(|key, _| shape.contains_key(key));
        for (key, value) in value.iter_mut() {
            if let Some(shape) = shape.get(key) {
                prune_tree(value, shape);
            }
        }
    }

    // we assume the config is serializable, if it isn't nothing is going to work anyway
    let mut value = json::to_value(&config).expect("container config is serializable");
    prune_tree(&mut value, &map);

    // the pruned config is the deserialized value of the pruned tree
    json::from_value(value).expect("pruned container config matches the schema")
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ServiceConfig(pub(super) oci::ContainerConfig);

impl State for ServiceConfig {
    type Target = Self;
}

impl Deref for ServiceConfig {
    type Target = oci::ContainerConfig;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ServiceConfig {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<oci::ContainerConfig> for ServiceConfig {
    /// Convert from an OCI container to a service configuration
    fn from(mut config: oci::ContainerConfig) -> Self {
        // Get the app_uuid for use in later operations
        let maybe_app_uuid = config.labels.remove(LABEL_APP_UUID);

        // Read the map of the composition this container was created from
        let config_map: json::Value = config
            .labels
            .remove(LABEL_CONFIG_MAP)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_else(|| json::json!({}));

        // Network aliases are tracked by value: helios appends the service
        // name to the target aliases at create time and the engine appends
        // the container id, so the label is the only record of what the
        // composition asked for.
        let target_aliases: HashMap<String, Vec<String>> = config
            .networks
            .keys()
            .map(|net_id| {
                let aliases = config
                    .labels
                    .remove(&format!("{LABEL_CONFIG_ALIASES}.{net_id}"))
                    .and_then(|s| json::from_str(&s).ok())
                    .unwrap_or_default();
                (net_id.clone(), aliases)
            })
            .collect();

        // Drop everything the composition did not define. The engine fills in
        // defaults for unset config fields, ulimits and annotations, and the
        // image contributes labels, environment and healthcheck fields.
        let mut config = prune_untracked(config, config_map);

        // A healthcheck left with no fields means the composition set
        // `healthcheck: {}`, which defers to the image's HEALTHCHECK
        if config.healthcheck.as_ref().is_some_and(|hc| hc.is_empty()) {
            config.healthcheck = None;
        }

        let namespace = maybe_app_uuid.map(LocalNamespace::from);

        config.networks = std::mem::take(&mut config.networks)
            .into_iter()
            .map(|(net_id, mut net_config)| {
                // keep only aliases that are in the target state
                net_config.aliases.retain(|alias| {
                    target_aliases
                        .get(&net_id)
                        .is_some_and(|aliases| aliases.contains(alias))
                });

                // De-namespace network names by stripping the app_uuid suffix
                let net_name = match &namespace {
                    Some(namespace) => namespace.to_entity(&net_id),
                    None => net_id,
                };

                (net_name, net_config)
            })
            .collect();

        // De-namespace volume mount sources by stripping the app_uuid suffix
        if let Some(namespace) = namespace {
            config.volumes = std::mem::take(&mut config.volumes)
                .into_iter()
                .map(|mut mount| {
                    if let Mount::Volume { source, .. } = &mut mount {
                        *source = namespace.to_entity(source);
                    }
                    mount
                })
                .collect();
        }

        Self(config)
    }
}

impl ServiceConfig {
    /// Converts the service config into container configuration
    ///
    /// Because some configurations may be defined on the image and the composition,
    /// this stores the shape of the composition-defined config in the
    /// [`LABEL_CONFIG_SHAPE`] label. When reading the container state, the shape is
    /// used to tell apart what the composition set from what the engine and the
    /// image filled in.
    pub fn into_oci_config(
        self,
        svc_id: u32,
        svc_name: &str,
        app_uuid: &Uuid,
        depends_on: &super::DependsOn,
    ) -> oci::ContainerConfig {
        let mut config = self.0;
        let namespace = LocalNamespace::from(app_uuid.as_str());

        // Namespace volume mount sources so they match the volumes created under the app
        config.volumes = std::mem::take(&mut config.volumes)
            .into_iter()
            .map(|mut mount| {
                if let Mount::Volume { source, .. } = &mut mount {
                    *source = namespace.to_identifier(source);
                }
                mount
            })
            .collect();

        // Namespace network names so they match the networks created under the app
        config.networks = std::mem::take(&mut config.networks)
            .into_iter()
            .map(|(net_name, net_config)| (namespace.to_identifier(&net_name), net_config))
            .collect();

        // Build a map of the composition-defined config before adding
        // anything of our own below
        let config_map = build_map(&config);

        // Store the target aliases per network, as both the engine and the
        // code below insert aliases that must not be read back into the state
        for (net_id, net_config) in &config.networks {
            config.labels.insert(
                format!("{LABEL_CONFIG_ALIASES}.{net_id}"),
                json::to_string(&net_config.aliases).expect("aliases are serializable"),
            );
        }

        let labels = &mut config.labels;
        labels.insert(LABEL_CONFIG_MAP.to_string(), config_map.to_string());

        // Set app and service metadata as labels when creating the container
        labels.insert(LABEL_SUPERVISED.to_string(), "".to_string());
        labels.insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
        labels.insert(LABEL_SERVICE_NAME.to_string(), svc_name.to_string());
        labels.insert(LABEL_SERVICE_ID.to_string(), svc_id.to_string());

        if !depends_on.is_empty()
            && let Ok(encoded) = json::to_string(depends_on)
        {
            labels.insert(LABEL_DEPENDS_ON.to_string(), encoded);
        }

        // add BALENA_ env vars that are tied to the container lifetime
        config
            .environment
            .insert(ENV_APP_UUID.to_string(), Some(app_uuid.as_str().into()));
        config
            .environment
            .insert(ENV_SERVICE_NAME.to_string(), Some(svc_name.into()));

        // insert the current service name as an alias on every network
        // so it can be referenced by name from other containers
        for net_config in config.networks.values_mut() {
            net_config.aliases.push(svc_name.to_string());
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn make_uuid() -> Uuid {
        Uuid::from("test-app-uuid")
    }

    #[test]
    fn round_trips_a_fully_populated_config() {
        let original = oci::ContainerConfig {
            annotations: HashMap::from([("com.example.foo".to_string(), "bar".to_string())]),
            cap_add: Vec::from(["NET_ADMIN".to_string()]),
            cap_drop: Vec::from(["MKNOD".to_string()]),
            command: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            cgroup: Some(oci::Cgroup::Host),
            cgroup_parent: Some("/custom".to_string()),
            cpuset: Some("0-3".to_string()),
            cpu_rt_period: 1_000_000,
            cpu_rt_runtime: 950_000,
            cpu_shares: 2048,
            devices: Vec::from([oci::DeviceMapping {
                source: "/dev/ttyUSB0".to_string(),
                target: "/dev/ttyUSB0".to_string(),
                permissions: "rwm".to_string(),
            }]),
            dns: Vec::from(["1.1.1.1".to_string()]),
            domainname: Some("example.com".to_string()),
            entrypoint: Some(vec!["/entrypoint.sh".to_string()]),
            environment: [("DEBUG".to_string(), "1")].into_iter().collect(),
            extra_hosts: HashMap::from([("db".to_string(), "10.0.0.2".to_string())]),
            healthcheck: Some(oci::Healthcheck {
                test: Some(vec!["CMD".to_string(), "true".to_string()]),
                interval: Some(30_000_000_000),
                timeout: Some(5_000_000_000),
                retries: Some(3),
                ..Default::default()
            }),
            hostname: Some("my-host".to_string()),
            init: Some(true),
            labels: HashMap::from([("com.example.role".to_string(), "db".to_string())]),
            mem_limit: 1073741824,
            mem_reservation: 536870912,
            nano_cpus: 1_500_000_000,
            networks: [(
                "default".to_string(),
                oci::NetworkSettings {
                    aliases: Vec::from(["db".to_string()]),
                    ipv4_address: Some("10.0.0.2".to_string()),
                    mac_address: Some("02:42:ac:11:00:02".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            oom_score_adj: Some(-500),
            pids_limit: Some(100),
            ports: ["8080:80", "127.0.0.1:5353:53/udp"]
                .into_iter()
                .map(|p| p.parse().unwrap())
                .collect(),
            privileged: true,
            restart_policy: oci::RestartPolicy::OnFailure {
                max_retries: Some(3),
            },
            runtime: Some("runc".to_string()),
            shm_size: Some(67108864),
            stop_grace_period: Some(30),
            stop_signal: Some("SIGTERM".to_string()),
            sysctls: HashMap::from([("net.ipv4.ip_forward".to_string(), "1".to_string())]),
            tmpfs: HashMap::from([(
                "/run".to_string(),
                oci::TmpfsOptions {
                    mode: Some("755".to_string()),
                    ..Default::default()
                },
            )]),
            ulimits: HashMap::from([(
                "nofile".to_string(),
                oci::Ulimit {
                    soft: 1024,
                    hard: 2048,
                },
            )]),
            user: Some("1000:1000".to_string()),
            userns_mode: Some("host".to_string()),
            uts: Some("host".to_string()),
            volumes: BTreeSet::from([
                oci::Mount::Volume {
                    target: "/data".to_string(),
                    source: "data".to_string(),
                    read_only: false,
                    nocopy: false,
                    subpath: None,
                },
                oci::Mount::Bind {
                    target: "/etc/hosts".to_string(),
                    source: "/etc/hosts".to_string(),
                    read_only: true,
                    propagation: Default::default(),
                    create_host_path: false,
                },
            ]),
            working_dir: Some("/app".to_string()),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        // Everything the composition defined comes back untouched, and
        // everything helios added on the way out is gone
        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.0, original);
    }

    #[test]
    fn drops_config_fields_when_not_in_label_config_shape() {
        // Simulate an inspect where the engine/image filled in values the
        // composition never requested. Fields that default to ""/0 are
        // filtered in helios-oci during inspect.
        let labels = HashMap::from([(LABEL_CONFIG_MAP.to_string(), "{}".to_string())]);
        let inspected = oci::ContainerConfig {
            command: Some(vec!["/bin/sh".to_string()]),
            entrypoint: Some(vec!["/docker-entrypoint.sh".to_string()]),
            healthcheck: Some(oci::Healthcheck {
                test: Some(vec!["CMD".to_string(), "image-default".to_string()]),
                ..Default::default()
            }),
            hostname: Some("a1b2c3d4e5f6".to_string()),
            init: Some(true),
            pids_limit: Some(100),
            runtime: Some("runc".to_string()),
            shm_size: Some(67108864),
            stop_grace_period: Some(10),
            stop_signal: Some("SIGTERM".to_string()),
            user: Some("root".to_string()),
            working_dir: Some("/".to_string()),
            labels,
            ..Default::default()
        };
        let svc = ServiceConfig::from(inspected);
        assert_eq!(svc.command, None);
        assert_eq!(svc.entrypoint, None);
        assert_eq!(svc.healthcheck, None);
        assert_eq!(svc.hostname, None);
        assert_eq!(svc.init, None);
        assert_eq!(svc.pids_limit, None);
        assert_eq!(svc.runtime, None);
        assert_eq!(svc.shm_size, None);
        assert_eq!(svc.stop_grace_period, None);
        assert_eq!(svc.stop_signal, None);
        assert_eq!(svc.user, None);
        assert_eq!(svc.working_dir, None);
    }

    #[test]
    fn preserves_annotations_across_round_trip() {
        let original = oci::ContainerConfig {
            annotations: HashMap::from([("com.example.foo".to_string(), "bar".to_string())]),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        // Simulate the engine attaching its own annotations to the container
        with_labels.annotations.insert(
            "io.podman.annotations.autoremove".to_string(),
            "FALSE".to_string(),
        );

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.annotations, original.annotations);
    }

    #[test]
    fn drops_engine_added_annotations_when_none_defined() {
        // Without a composition-defined annotation the tracking label is an
        // empty list, so engine-added annotations are dropped on read
        let svc = ServiceConfig(oci::ContainerConfig::default());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());
        with_labels
            .annotations
            .insert("io.container.manager".to_string(), "libpod".to_string());

        let back = ServiceConfig::from(with_labels);
        assert!(back.annotations.is_empty());
    }

    #[test]
    fn preserves_ulimits_across_round_trip() {
        let original = oci::ContainerConfig {
            ulimits: HashMap::from([(
                "nofile".to_string(),
                oci::Ulimit {
                    soft: 1024,
                    hard: 2048,
                },
            )]),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        // Simulate the engine adding its own default ulimits
        with_labels.ulimits.insert(
            "nproc".to_string(),
            oci::Ulimit {
                soft: 4194304,
                hard: 4194304,
            },
        );

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.ulimits, original.ulimits);
    }

    #[test]
    fn drops_engine_added_ulimits_when_none_defined() {
        let svc = ServiceConfig(oci::ContainerConfig::default());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());
        with_labels.ulimits.insert(
            "nofile".to_string(),
            oci::Ulimit {
                soft: 1048576,
                hard: 1048576,
            },
        );

        let back = ServiceConfig::from(with_labels);
        assert!(back.ulimits.is_empty());
    }

    #[test]
    fn preserves_ports_across_round_trip() {
        let original = oci::ContainerConfig {
            ports: ["8080:80", "127.0.0.1:5353:53/udp", "443"]
                .into_iter()
                .map(|p| p.parse().unwrap())
                .collect(),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.ports, original.ports);
    }

    #[test]
    fn drops_values_for_fields_the_composition_never_set() {
        // Every serialized field is covered by the shape, including the
        // scalars and lists that no tracking label used to cover
        let svc = ServiceConfig(oci::ContainerConfig::default());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        with_labels.cpu_shares = 1024;
        with_labels.mem_reservation = 536870912;
        with_labels.ports = ["8080:80".parse().unwrap()].into_iter().collect();

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.cpu_shares, 0);
        assert_eq!(back.mem_reservation, 0);
        assert!(back.ports.is_empty());
    }

    #[test]
    fn drops_engine_added_network_aliases_and_mac_address() {
        let networks = [(
            "default".to_string(),
            oci::NetworkSettings {
                aliases: Vec::from(["db".to_string()]),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();
        let original = oci::ContainerConfig {
            networks,
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        // Simulate the engine adding the container id as an alias and
        // assigning a mac address the composition never asked for
        let net_config = with_labels
            .networks
            .get_mut("default_test-app-uuid")
            .expect("network is namespaced by app uuid");
        net_config.aliases.push("a1b2c3d4e5f6".to_string());
        net_config.mac_address = Some("02:42:ac:11:00:02".to_string());

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.networks, original.networks);
    }

    #[test]
    fn preserves_composition_defined_mac_address() {
        let networks = [(
            "default".to_string(),
            oci::NetworkSettings {
                mac_address: Some("02:42:ac:11:00:02".to_string()),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();
        let original = oci::ContainerConfig {
            networks,
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.networks, original.networks);
    }

    #[test]
    fn drops_engine_inherited_healthcheck_subfields() {
        let original = oci::ContainerConfig {
            healthcheck: Some(oci::Healthcheck {
                test: Some(vec!["CMD-SHELL".to_string(), "echo ok".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        // Simulate the engine filling in fields defined in image HEALTHCHECK
        let hc = with_labels.healthcheck.as_mut().unwrap();
        hc.interval = Some(10_000_000_000);
        hc.timeout = Some(3_000_000_000);
        hc.start_period = Some(2_000_000_000);
        hc.retries = Some(3);

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.healthcheck, original.healthcheck);
    }

    #[test]
    fn collapses_healthcheck_with_no_tracked_subfields() {
        // Target defines healthcheck: {} (valid yaml)
        let original = oci::ContainerConfig {
            healthcheck: Some(oci::Healthcheck::default()),
            ..Default::default()
        };
        let svc = ServiceConfig(original);
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default());

        // Engine inherits image's full HEALTHCHECK
        let hc = with_labels.healthcheck.as_mut().unwrap();
        hc.test = Some(vec!["CMD-SHELL".to_string(), "from-image".to_string()]);
        hc.interval = Some(10_000_000_000);

        // No fields are tracked by helios
        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.healthcheck, None);
    }
}
