use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};
use serde_json as json;

use crate::common_types::Uuid;
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_ID, LABEL_SERVICE_NAME, LABEL_SUPERVISED};
use crate::oci::{self, IpcMode, LocalNamespace, Mount, Namespace, NetworkMode, PidMode};

const LABEL_CONFIG_MAP: &str = "io.balena.private.config.map";
pub(super) const LABEL_DEPENDS_ON: &str = "io.balena.private.depends-on";
const ENV_APP_UUID: &str = "BALENA_APP_UUID";
const ENV_SERVICE_NAME: &str = "BALENA_SERVICE_NAME";

/// Map of the object keys present in the composition-defined container config, recursively.
///
/// The map is built based on the serialization shape of the [`oci::ContainerConfig`]. Fields
/// skipped during serialization are assumed to not be used in the container creation.
///
/// Serialized, the map will have the following shape
///
/// ```json
/// {"foo": null, "bar": {}, "baz": {...}, "network_mode": "service:db"}
/// ```
/// this is read as
/// - `foo` is a leaf, the composition set the value and the key should be read back from the engine
/// - `bar` has sub-keys, but none were set on the composition, so retrieved sub-keys should be dropped
/// - `baz` has sub-keys, recurse over them to determine what was set by the composition
/// - `network_mode` is a leaf holding the composition value, which is restored over the one read
///   from the engine
/// - any key absent from the map is skipped on read back
///
/// Leaves hold the composition value for the values the engine does not report back as they
/// were set
///
/// - service references in `network_mode`, `ipc` and `pid`, which are resolved to a container id
///   before the engine sees them, and
/// - the target aliases of each network (`networks.<id>.aliases`). Helios appends the service
///   name to the target aliases at create time and the engine appends the container id, so
///   neither can be told apart from what the composition asked for.
#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(transparent)]
struct ConfigMap(json::Map<String, json::Value>);

impl ConfigMap {
    /// Serialize the config to a JSON object
    fn map_from(config: &oci::ContainerConfig) -> json::Map<String, json::Value> {
        // we assume the config is serializable, if it isn't nothing is going to work anyway
        match json::to_value(config).expect("container config is serializable") {
            json::Value::Object(map) => map,
            _ => unreachable!("container config serializes to an object"),
        }
    }

    /// Build the map of the keys present in the given config
    fn build(config: &oci::ContainerConfig) -> Self {
        // Map the keys in the `value`, recursively
        fn shape_of(value: &json::Map<String, json::Value>) -> json::Map<String, json::Value> {
            value
                .iter()
                .map(|(key, value)| {
                    let shape = match value {
                        json::Value::Object(map) => json::Value::Object(shape_of(map)),
                        _ => json::Value::Null,
                    };
                    (key.clone(), shape)
                })
                .collect()
        }

        let mut map = shape_of(&Self::map_from(config));

        // For those keys that require recording the value, set them after the shape is ready
        if let Some(mode @ NetworkMode::Service(_)) = &config.network_mode {
            map.insert("network_mode".to_string(), json::json!(mode));
        }
        if let Some(mode @ IpcMode::Service(_)) = &config.ipc {
            map.insert("ipc".to_string(), json::json!(mode));
        }
        if let Some(mode @ PidMode::Service(_)) = &config.pid {
            map.insert("pid".to_string(), json::json!(mode));
        }
        if let Some(networks) = map.get_mut("networks").and_then(json::Value::as_object_mut) {
            for (net_id, net_config) in &config.networks {
                if !net_config.aliases.is_empty()
                    && let Some(net_map) = networks
                        .get_mut(net_id)
                        .and_then(json::Value::as_object_mut)
                {
                    net_map.insert("aliases".to_string(), json::json!(net_config.aliases));
                }
            }
        }

        Self(map)
    }

    /// Recover the composition-defined config from the config reported by the engine
    ///
    /// Keys of the input config that are not part of the map are removed. Anything the
    /// composition sets will round-trip untouched, while defaults filled by the engine or
    /// inherited from the image will be dropped.
    ///
    /// If the map does not describe a valid config, e.g. it was written by a different version,
    /// the config is not pruned.
    ///
    /// The tracked values are then restored. Service references replace the container
    /// the engine resolved them to. Network aliases ar reduced to the target aliases.
    fn restore(&self, config: oci::ContainerConfig) -> oci::ContainerConfig {
        /// Remove the object keys of `value` that are not part of `shape`.
        fn prune_tree(
            value: &mut json::Map<String, json::Value>,
            shape: &json::Map<String, json::Value>,
        ) {
            value.retain(|key, _| shape.contains_key(key));
            for (key, value) in value.iter_mut() {
                if let (json::Value::Object(value), Some(json::Value::Object(shape))) =
                    (value, shape.get(key))
                {
                    prune_tree(value, shape);
                }
            }
        }

        let mut value = Self::map_from(&config);
        prune_tree(&mut value, &self.0);

        // the pruned config is the deserialized value of the pruned tree
        let mut config = json::from_value(json::Value::Object(value)).unwrap_or(config);

        if matches!(config.network_mode, Some(NetworkMode::Container(_)))
            && let Some(mode) = self.tracked("network_mode")
        {
            config.network_mode = Some(mode);
        }
        if matches!(config.ipc, Some(IpcMode::Container(_)))
            && let Some(mode) = self.tracked("ipc")
        {
            config.ipc = Some(mode);
        }
        if matches!(config.pid, Some(PidMode::Container(_)))
            && let Some(mode) = self.tracked("pid")
        {
            config.pid = Some(mode);
        }

        for (net_id, net_config) in config.networks.iter_mut() {
            let target_aliases = self.aliases(net_id);
            net_config
                .aliases
                .retain(|alias| target_aliases.contains(&alias.as_str()));
        }

        config
    }

    /// The composition value tracked for a top level key, if any
    fn tracked<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        T::deserialize(self.0.get(key)?).ok()
    }

    /// The target aliases of the given network, as set by the composition
    fn aliases(&self, net_id: &str) -> Vec<&str> {
        self.0
            .get("networks")
            .and_then(|networks| networks.get(net_id))
            .and_then(|network| network.get("aliases"))
            .and_then(json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(json::Value::as_str)
            .collect()
    }
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
        let config_map: ConfigMap = config
            .labels
            .remove(LABEL_CONFIG_MAP)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_default();

        // Drop everything the composition did not define. The engine fills in
        // defaults for unset config fields, ulimits and annotations, and the
        // image contributes labels, environment and healthcheck fields.
        // This step also restores lossy values, e.g. network aliases and `service:<name>` modes.
        let mut config = config_map.restore(config);

        // A healthcheck left with no fields means the composition set
        // `healthcheck: {}`, which defers to the image's HEALTHCHECK
        if config.healthcheck.as_ref().is_some_and(|hc| hc.is_empty()) {
            config.healthcheck = None;
        }

        let namespace = maybe_app_uuid.map(LocalNamespace::from);

        config.networks = std::mem::take(&mut config.networks)
            .into_iter()
            .map(|(net_id, net_config)| {
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
    /// this stores a map of the composition-defined config in the [`LABEL_CONFIG_MAP`]
    /// label. When reading the container state, the map is used to tell apart what the
    /// composition set from what the engine and the image filled in.
    ///
    /// Volume mount sources are namespaced so they match the volumes created for the app, except
    /// for the sources listed in `external_volumes`, which refer to volumes that exist outside the
    /// app namespace and are used as given.
    pub fn into_oci_config(
        self,
        svc_id: u32,
        svc_name: &str,
        app_uuid: &Uuid,
        depends_on: &super::DependsOn,
        external_volumes: &[String],
    ) -> oci::ContainerConfig {
        let mut config = self.0;
        let namespace = LocalNamespace::from(app_uuid.as_str());

        // Namespace volume mount sources so they match the volumes created under the app
        config.volumes = std::mem::take(&mut config.volumes)
            .into_iter()
            .map(|mut mount| {
                if let Mount::Volume { source, .. } = &mut mount
                    && !external_volumes.iter().any(|name| name == source)
                {
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
        let config_map = ConfigMap::build(&config);

        let labels = &mut config.labels;
        labels.insert(
            LABEL_CONFIG_MAP.to_string(),
            json::to_string(&config_map).expect("config map is serializable"),
        );

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
    use std::collections::{BTreeSet, HashMap};

    fn make_uuid() -> Uuid {
        Uuid::from("test-app-uuid")
    }

    /// Render a config for the engine and read it back, as install and inspect do.
    fn round_trip(config: oci::ContainerConfig) -> (oci::ContainerConfig, ServiceConfig) {
        let rendered =
            ServiceConfig(config).into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        let back = ServiceConfig::from(rendered.clone());
        (rendered, back)
    }

    #[test]
    fn tracks_values_the_engine_does_not_report_in_the_config_map() {
        let rendered = ServiceConfig(oci::ContainerConfig {
            ipc: Some(IpcMode::Service("db".to_string())),
            pid: Some(PidMode::Host),
            networks: [(
                "default".to_string(),
                oci::NetworkSettings {
                    aliases: Vec::from(["db".to_string()]),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);

        // only service references are tracked in the map, and the aliases are the ones
        // from the composition, without the service name added by helios
        let map: json::Value = json::from_str(&rendered.labels[LABEL_CONFIG_MAP]).unwrap();
        assert_eq!(map["ipc"], json::json!("service:db"));
        assert_eq!(map["pid"], json::Value::Null);
        assert_eq!(
            map["networks"]["default_test-app-uuid"]["aliases"],
            json::json!(["db"])
        );
    }

    #[test]
    fn service_network_mode_survives_a_round_trip() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            network_mode: Some(NetworkMode::Service("db".to_string())),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);

        // the reference stays in composition form, `Container::create` resolves it
        assert_eq!(
            rendered.network_mode,
            Some(NetworkMode::Service("db".to_string()))
        );

        // and the engine reports the container it resolved to
        rendered.network_mode = Some(NetworkMode::Container("0123abcd".to_string()));
        let back = ServiceConfig::from(rendered);
        assert_eq!(
            back.network_mode,
            Some(NetworkMode::Service("db".to_string()))
        );
    }

    #[test]
    fn a_service_network_mode_the_engine_did_not_apply_is_read_as_reported() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            network_mode: Some(NetworkMode::Service("db".to_string())),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered.network_mode = Some(NetworkMode::Host);

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.network_mode, Some(NetworkMode::Host));
    }

    #[test]
    fn a_network_mode_the_engine_did_not_apply_is_read_as_reported() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            network_mode: Some(NetworkMode::Host),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered.network_mode = Some(NetworkMode::None);

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.network_mode, Some(NetworkMode::None));
    }

    #[test]
    fn an_engine_reported_network_mode_is_not_read_when_the_composition_set_none() {
        let mut rendered = ServiceConfig(oci::ContainerConfig::default()).into_oci_config(
            1,
            "web",
            &make_uuid(),
            &Default::default(),
            &[],
        );
        rendered.network_mode = Some(NetworkMode::Bridge);

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.network_mode, None);
    }

    #[test]
    fn user_networks_reported_with_a_bridge_mode_are_kept_when_the_composition_set_none() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            networks: [("default_app".to_string(), Default::default())]
                .into_iter()
                .collect(),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered.network_mode = Some(NetworkMode::Bridge);

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.network_mode, None);
        assert_eq!(back.networks.len(), 1);
    }

    #[test]
    fn networks_reported_with_a_requested_bridge_mode_are_dropped() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            network_mode: Some(NetworkMode::Bridge),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered
            .networks
            .insert("podman".to_string(), Default::default());

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.network_mode, Some(NetworkMode::Bridge));
        assert!(back.networks.is_empty());
    }

    #[test]
    fn host_network_mode_survives_a_round_trip() {
        let (_, back) = round_trip(oci::ContainerConfig {
            network_mode: Some(NetworkMode::Host),
            ..Default::default()
        });
        assert_eq!(back.network_mode, Some(NetworkMode::Host));
    }

    #[test]
    fn bridge_network_mode_survives_a_round_trip() {
        let (_, back) = round_trip(oci::ContainerConfig {
            network_mode: Some(NetworkMode::Bridge),
            ..Default::default()
        });
        assert_eq!(back.network_mode, Some(NetworkMode::Bridge));
    }

    #[test]
    fn service_ipc_survives_a_round_trip() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            ipc: Some(IpcMode::Service("db".to_string())),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);

        // the reference stays in composition form, `Container::create` resolves it
        assert_eq!(rendered.ipc, Some(IpcMode::Service("db".to_string())));

        // and the engine reports the container it resolved to
        rendered.ipc = Some(IpcMode::Container("0123abcd".to_string()));
        let back = ServiceConfig::from(rendered);
        assert_eq!(back.ipc, Some(IpcMode::Service("db".to_string())));
    }

    #[test]
    fn a_service_ipc_the_engine_did_not_apply_is_read_as_reported() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            ipc: Some(IpcMode::Service("db".to_string())),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered.ipc = Some(IpcMode::Other("private".to_string()));

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.ipc, Some(IpcMode::Other("private".to_string())));
    }

    #[test]
    fn shareable_ipc_survives_a_round_trip() {
        let (_, back) = round_trip(oci::ContainerConfig {
            ipc: Some(IpcMode::Shareable),
            ..Default::default()
        });
        assert_eq!(back.ipc, Some(IpcMode::Shareable));
    }

    #[test]
    fn host_ipc_survives_a_round_trip() {
        let (_, back) = round_trip(oci::ContainerConfig {
            ipc: Some(IpcMode::Host),
            ..Default::default()
        });
        assert_eq!(back.ipc, Some(IpcMode::Host));
    }

    #[test]
    fn none_ipc_survives_a_round_trip() {
        let (_, back) = round_trip(oci::ContainerConfig {
            ipc: Some(IpcMode::None),
            ..Default::default()
        });
        assert_eq!(back.ipc, Some(IpcMode::None));
    }

    #[test]
    fn an_ipc_the_engine_did_not_apply_is_read_as_reported() {
        // the engine falls back to its default when it does not support the mode
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            ipc: Some(IpcMode::Shareable),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered.ipc = Some(IpcMode::Other("private".to_string()));

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.ipc, Some(IpcMode::Other("private".to_string())));
    }

    #[test]
    fn an_engine_default_ipc_is_not_read_as_a_mode() {
        // with no mode given the engine reports its default
        let mut rendered = ServiceConfig(oci::ContainerConfig::default()).into_oci_config(
            1,
            "web",
            &make_uuid(),
            &Default::default(),
            &[],
        );
        rendered.ipc = Some(IpcMode::Other("private".to_string()));

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.ipc, None);
    }

    #[test]
    fn service_pid_survives_a_round_trip() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            pid: Some(PidMode::Service("db".to_string())),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        // the reference stays in composition form, `Container::create` resolves it
        assert_eq!(rendered.pid, Some(PidMode::Service("db".to_string())));

        // and the engine reports the container it resolved to
        rendered.pid = Some(PidMode::Container("0123abcd".to_string()));
        let back = ServiceConfig::from(rendered);
        assert_eq!(back.pid, Some(PidMode::Service("db".to_string())));
    }

    #[test]
    fn a_service_pid_the_engine_did_not_apply_is_read_as_reported() {
        let mut rendered = ServiceConfig(oci::ContainerConfig {
            pid: Some(PidMode::Service("db".to_string())),
            ..Default::default()
        })
        .into_oci_config(1, "web", &make_uuid(), &Default::default(), &[]);
        rendered.pid = Some(PidMode::Other("private".to_string()));

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.pid, Some(PidMode::Other("private".to_string())));
    }

    #[test]
    fn host_pid_survives_a_round_trip() {
        let (_, back) = round_trip(oci::ContainerConfig {
            pid: Some(PidMode::Host),
            ..Default::default()
        });
        assert_eq!(back.pid, Some(PidMode::Host));
    }

    #[test]
    fn an_engine_default_pid_is_not_read_as_a_mode() {
        // with no mode given the engine reports its default
        let mut rendered = ServiceConfig(oci::ContainerConfig::default()).into_oci_config(
            1,
            "web",
            &make_uuid(),
            &Default::default(),
            &[],
        );
        rendered.pid = Some(PidMode::Other("private".to_string()));

        let back = ServiceConfig::from(rendered);
        assert_eq!(back.pid, None);
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
            device_cgroup_rules: Vec::from(["c 13:* rmw".to_string()]),
            devices: Vec::from([oci::DeviceMapping {
                source: "/dev/ttyUSB0".to_string(),
                target: "/dev/ttyUSB0".to_string(),
                permissions: "rwm".to_string(),
            }]),
            dns: Vec::from(["1.1.1.1".to_string()]),
            dns_opt: Vec::from(["ndots:2".to_string()]),
            dns_search: Vec::from(["example.com".to_string()]),
            domainname: Some("example.com".to_string()),
            entrypoint: Some(vec!["/entrypoint.sh".to_string()]),
            environment: [("DEBUG".to_string(), "1")].into_iter().collect(),
            extra_hosts: HashMap::from([("db".to_string(), "10.0.0.2".to_string())]),
            group_add: Vec::from(["audio".to_string()]),
            healthcheck: Some(oci::Healthcheck {
                test: Some(vec!["CMD".to_string(), "true".to_string()]),
                interval: Some(30_000_000_000),
                timeout: Some(5_000_000_000),
                retries: Some(3),
                ..Default::default()
            }),
            hostname: Some("my-host".to_string()),
            init: Some(true),
            ipc: Some(IpcMode::Shareable),
            labels: HashMap::from([("com.example.role".to_string(), "db".to_string())]),
            mem_limit: 1073741824,
            mem_reservation: 536870912,
            nano_cpus: 1_500_000_000,
            networks: [(
                "default".to_string(),
                oci::NetworkSettings {
                    aliases: Vec::from(["db".to_string()]),
                    ipv4_address: Some("10.0.0.2".to_string()),
                    ipv6_address: Some("fd00::2".to_string()),
                    link_local_ips: Vec::from(["169.254.0.2".to_string()]),
                    mac_address: Some("02:42:ac:11:00:02".to_string()),
                    driver_opts: HashMap::from([("k".to_string(), "v".to_string())]),
                    gw_priority: Some(10),
                },
            )]
            .into_iter()
            .collect(),
            oom_score_adj: Some(-500),
            pid: Some(PidMode::Host),
            pids_limit: Some(100),
            ports: ["8080:80", "127.0.0.1:5353:53/udp"]
                .into_iter()
                .map(|p| p.parse().unwrap())
                .collect(),
            privileged: true,
            read_only: true,
            restart_policy: oci::RestartPolicy::OnFailure {
                max_retries: Some(3),
            },
            runtime: Some("runc".to_string()),
            security_opt: Vec::from(["no-new-privileges".to_string()]),
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
            tty: true,
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
                oci::Mount::Volume {
                    target: "/shared".to_string(),
                    source: "shared".to_string(),
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
        let with_labels = svc.into_oci_config(
            1,
            "svc",
            &make_uuid(),
            &Default::default(),
            &["shared".to_string()],
        );

        // Everything the composition defined comes back untouched, and
        // everything helios added on the way out is gone
        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.0, original);
    }

    #[test]
    fn round_trips_a_network_mode_config() {
        // `network_mode` is mutually exclusive with `networks`
        let original = oci::ContainerConfig {
            network_mode: Some(NetworkMode::Host),
            ..Default::default()
        };
        let with_labels = ServiceConfig(original.clone()).into_oci_config(
            1,
            "svc",
            &make_uuid(),
            &Default::default(),
            &[],
        );
        assert_eq!(ServiceConfig::from(with_labels).0, original);
    }

    #[test]
    fn returns_the_config_unpruned_when_the_map_does_not_match_the_schema() {
        // `restart_policy` cannot be read back without its `name`
        let config = oci::ContainerConfig {
            cpu_shares: 1024,
            restart_policy: oci::RestartPolicy::OnFailure {
                max_retries: Some(3),
            },
            ..Default::default()
        };
        let map: ConfigMap = json::from_value(json::json!({"restart_policy": {}})).unwrap();

        assert_eq!(map.restore(config.clone()), config);
    }

    #[test]
    fn tolerates_a_map_with_empty_nested_objects() {
        // The map may describe a composition that set none of the nested keys
        let populated = oci::ContainerConfig {
            healthcheck: Some(oci::Healthcheck {
                test: Some(vec!["CMD".to_string(), "true".to_string()]),
                ..Default::default()
            }),
            networks: [(
                "default_test-app-uuid".to_string(),
                oci::NetworkSettings {
                    ipv4_address: Some("10.0.0.2".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            restart_policy: oci::RestartPolicy::OnFailure {
                max_retries: Some(3),
            },
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
            ..Default::default()
        };
        let map: ConfigMap = json::from_value(json::json!({
            "healthcheck": {},
            "networks": {"default_test-app-uuid": {}},
            "restart_policy": {"name": null},
            "tmpfs": {"/run": {}},
            "ulimits": {"nofile": {}},
        }))
        .unwrap();

        let pruned = map.restore(populated);
        assert_eq!(pruned.networks["default_test-app-uuid"].ipv4_address, None);
        assert_eq!(pruned.tmpfs["/run"].mode, None);
        assert_eq!(pruned.ulimits["nofile"], oci::Ulimit::default());
    }

    #[test]
    fn drops_config_fields_when_not_in_config_map() {
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
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

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
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);
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
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

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
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);
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
        let with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.ports, original.ports);
    }

    #[test]
    fn drops_values_for_fields_the_composition_never_set() {
        // Every serialized field is covered by the map, including the
        // scalars and lists that no tracking label used to cover
        let svc = ServiceConfig(oci::ContainerConfig::default());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

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
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

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
    fn drops_engine_inherited_healthcheck_subfields() {
        let original = oci::ContainerConfig {
            healthcheck: Some(oci::Healthcheck {
                test: Some(vec!["CMD-SHELL".to_string(), "echo ok".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

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
    fn namespaces_volume_mount_sources_except_external_ones() {
        let original = oci::ContainerConfig {
            volumes: [
                Mount::Volume {
                    target: "/data".to_string(),
                    source: "app-volume".to_string(),
                    read_only: false,
                    nocopy: false,
                    subpath: None,
                },
                Mount::Volume {
                    target: "/shared".to_string(),
                    source: "shared-volume".to_string(),
                    read_only: false,
                    nocopy: false,
                    subpath: None,
                },
                Mount::Bind {
                    target: "/etc/machine-id".to_string(),
                    source: "/etc/machine-id".to_string(),
                    read_only: true,
                    propagation: oci::BindPropagation::default(),
                    create_host_path: false,
                },
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let svc = ServiceConfig(original);
        let with_labels = svc.into_oci_config(
            1,
            "svc",
            &make_uuid(),
            &Default::default(),
            &["shared-volume".to_string()],
        );

        let sources: Vec<(&str, &str)> = with_labels
            .volumes
            .iter()
            .map(|m| match m {
                Mount::Volume { target, source, .. } => (target.as_str(), source.as_str()),
                Mount::Bind { target, source, .. } => (target.as_str(), source.as_str()),
                _ => unreachable!("unexpected mount type"),
            })
            .collect();

        assert_eq!(
            sources,
            vec![
                ("/data", "app-volume_test-app-uuid"),
                // external volumes live outside the app namespace
                ("/shared", "shared-volume"),
                ("/etc/machine-id", "/etc/machine-id"),
            ]
        );
    }

    #[test]
    fn reads_back_external_volume_mount_sources_unchanged() {
        let original = oci::ContainerConfig {
            volumes: [Mount::Volume {
                target: "/shared".to_string(),
                source: "shared-volume".to_string(),
                read_only: false,
                nocopy: false,
                subpath: None,
            }]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let with_labels = svc.into_oci_config(
            1,
            "svc",
            &make_uuid(),
            &Default::default(),
            &["shared-volume".to_string()],
        );

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.volumes, original.volumes);
    }

    #[test]
    fn collapses_healthcheck_with_no_tracked_subfields() {
        // Target defines healthcheck: {} (valid yaml)
        let original = oci::ContainerConfig {
            healthcheck: Some(oci::Healthcheck::default()),
            ..Default::default()
        };
        let svc = ServiceConfig(original);
        let mut with_labels = svc.into_oci_config(1, "svc", &make_uuid(), &Default::default(), &[]);

        // Engine inherits image's full HEALTHCHECK
        let hc = with_labels.healthcheck.as_mut().unwrap();
        hc.test = Some(vec!["CMD-SHELL".to_string(), "from-image".to_string()]);
        hc.interval = Some(10_000_000_000);

        // No fields are tracked by helios
        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.healthcheck, None);
    }
}
