use std::collections::HashSet;
use std::ops::{Deref, DerefMut};

use mahler::state::State;
use serde::{Deserialize, Serialize};
use serde_json as json;

use crate::common_types::Uuid;
use crate::labels::{LABEL_APP_UUID, LABEL_SERVICE_ID, LABEL_SERVICE_NAME, LABEL_SUPERVISED};
use crate::oci::{self, LocalNamespace, Mount, Namespace};

const LABEL_CONFIG_FIELDS: &str = "io.balena.private.config.fields";
const LABEL_CONFIG_LABELS: &str = "io.balena.private.config.labels";
const LABEL_CONFIG_ENV: &str = "io.balena.private.config.env";
const LABEL_CONFIG_NETWORKS: &str = "io.balena.private.config.networks";
const ENV_APP_UUID: &str = "BALENA_APP_UUID";
const ENV_SERVICE_NAME: &str = "BALENA_SERVICE_NAME";

/// Per-field tracking for config fields whose value the engine or image
/// fills in when unset, so need to be tracked in LABEL_CONFIG_FIELDS.
trait WithTrackedFields {
    /// Set tracked fields to `None` unless their name appears in `fields`.
    fn remove_untracked(&mut self, fields: &HashSet<String>);

    /// Return the names of tracked fields whose value is currently `Some(_)`.
    fn collect_tracked(&self) -> Vec<&'static str>;
}

macro_rules! impl_field_tracking {
    ($ty:ty { $($name:ident),* $(,)? }) => {
        impl WithTrackedFields for $ty {
            fn remove_untracked(&mut self, fields: &HashSet<String>) {
                $(
                    if !fields.contains(stringify!($name)) {
                        self.$name = None;
                    }
                )*
            }

            fn collect_tracked(&self) -> Vec<&'static str> {
                let mut out = Vec::new();
                $(
                    if self.$name.is_some() {
                        out.push(stringify!($name));
                    }
                )*
                out
            }
        }
    };
}

impl_field_tracking!(oci::ContainerConfig {
    cgroup_parent,
    command,
    hostname,
    init,
    pids_limit,
    runtime,
    shm_size,
    stop_grace_period,
    stop_signal,
    oom_score_adj,
    user,
    uts,
    working_dir,
});

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
        let labels = &mut config.labels;

        // Get the app_uuid for use in later operations
        let maybe_app_uuid = labels.remove(LABEL_APP_UUID);

        // Read the list of fields defined in the composition used to create
        // this container
        let label_config_fields: HashSet<String> = labels
            .remove(LABEL_CONFIG_FIELDS)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_default();

        // Retain only environment variables that were defined in the composition
        let label_config_env: Vec<String> = labels
            .remove(LABEL_CONFIG_ENV)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_default();
        config
            .environment
            .retain(|k, _| label_config_env.contains(k));

        // De-namespace network names by stripping the app_uuid suffix
        if let Some(app_uuid) = maybe_app_uuid {
            let namespace = LocalNamespace::from(app_uuid);
            let networks = std::mem::take(&mut config.networks);
            config.networks = networks
                .into_iter()
                .map(|(net_id, mut net_config)| {
                    let net_name = namespace.to_entity(&net_id);

                    // get the list of target aliases from labels
                    let target_aliases: Vec<String> = labels
                        .remove(&format!("{LABEL_CONFIG_NETWORKS}.{net_name}.aliases"))
                        .and_then(|s| json::from_str(&s).ok())
                        .unwrap_or_default();

                    // keep only aliases that are in the target state
                    net_config
                        .aliases
                        .retain(|alias| target_aliases.contains(alias));

                    // The engine assigns a mac_address when one isn't provided.
                    // The label marks whether the target set one; if absent,
                    // drop whatever the engine reported so a round-trip
                    // doesn't see the engine-assigned address as a config change.
                    if labels
                        .remove(&format!("{LABEL_CONFIG_NETWORKS}.{net_name}.mac_address"))
                        .is_none()
                    {
                        net_config.mac_address = None;
                    }

                    (net_name, net_config)
                })
                .collect();

            // De-namespace volume mount sources by stripping the app_uuid suffix
            for mount in config.volumes.iter_mut() {
                if let Mount::Volume { source, .. } = mount {
                    *source = namespace.to_entity(source);
                }
            }
        }

        // Remove labels from the container that were not defined in
        // the composition. These are coming from the image and should not be
        // read into the service config
        let label_config_labels_value: Vec<String> = labels
            .remove(LABEL_CONFIG_LABELS)
            .and_then(|s| json::from_str(&s).ok())
            .unwrap_or_default();
        labels.retain(|k, _| label_config_labels_value.contains(k));

        // Drop fields not in the composition as the engine fills these in
        // with default values.
        config.remove_untracked(&label_config_fields);

        Self(config)
    }
}

impl ServiceConfig {
    /// Converts the service config into container configuration
    ///
    /// Because some configurations may be defined on the image and the the composition,
    /// this creates custom labels [`LABEL_CONFIG_FIELDS`], [`LABEL_CONFIG_LABELS`] containing a
    /// list of composition defined keys and labels. When reading the container state, these fields
    /// are used to determine if the specific field/label should be read back into the state.
    pub fn into_oci_config(
        self,
        svc_id: u32,
        svc_name: &str,
        app_uuid: &Uuid,
    ) -> oci::ContainerConfig {
        let mut config = self.0;

        // List of config fields coming from the composition. This is only necessary for fields that
        // may be shared between the image and service, since docker will use the image version as
        // the default unless overridden
        let label_config_fields_value = config
            .collect_tracked()
            .into_iter()
            .map(|s| json::Value::String(s.to_owned()))
            .collect::<json::Value>();

        let labels = &mut config.labels;

        // We create a label LABEL_CONFIG_LABELS containing user defined labels on the composition
        // we will use these when reading the state to remove labels coming from
        // the image
        let label_config_labels_value = labels
            .keys()
            .map(|s| json::Value::String(s.to_owned()))
            .collect::<json::Value>();
        labels.insert(
            LABEL_CONFIG_LABELS.to_string(),
            label_config_labels_value.to_string(),
        );

        // Store composition-defined environment variable keys
        let label_config_env_value = config
            .environment
            .keys()
            .map(|s| json::Value::String(s.to_owned()))
            .collect::<json::Value>();
        labels.insert(
            LABEL_CONFIG_ENV.to_string(),
            label_config_env_value.to_string(),
        );

        // add BALENA_ env vars that are tied to the container lifetime
        config
            .environment
            .insert(ENV_APP_UUID.to_string(), Some(app_uuid.as_str().into()));
        config
            .environment
            .insert(ENV_SERVICE_NAME.to_string(), Some(svc_name.into()));

        labels.insert(
            LABEL_CONFIG_FIELDS.to_string(),
            label_config_fields_value.to_string(),
        );

        // Set app and service metadata as labels when creating the container
        labels.insert(LABEL_SUPERVISED.to_string(), "".to_string());
        labels.insert(LABEL_APP_UUID.to_string(), app_uuid.to_string());
        labels.insert(LABEL_SERVICE_NAME.to_string(), svc_name.to_string());
        labels.insert(LABEL_SERVICE_ID.to_string(), svc_id.to_string());

        let namespace = LocalNamespace::from(app_uuid.as_str());

        // Namespace volume mount sources so they match the volumes created under the app
        for mount in config.volumes.iter_mut() {
            if let Mount::Volume { source, .. } = mount {
                *source = namespace.to_identifier(source);
            }
        }

        let networks = std::mem::take(&mut config.networks);
        for (net_name, mut net_config) in networks {
            // store the target aliases into a label as the engine may insert new aliases
            // that we want to remove when reading the container state
            labels.insert(
                format!("{LABEL_CONFIG_NETWORKS}.{net_name}.aliases"),
                net_config
                    .aliases
                    .iter()
                    .map(|s| json::Value::String(s.to_owned()))
                    .collect::<json::Value>()
                    .to_string(),
            );

            // mark whether the target set a mac_address so we can drop the
            // engine-assigned one on read when the composition didn't ask for it
            if net_config.mac_address.is_some() {
                labels.insert(
                    format!("{LABEL_CONFIG_NETWORKS}.{net_name}.mac_address"),
                    String::new(),
                );
            }

            let net_id = namespace.to_identifier(&net_name);

            // insert the current service name as an alias on the network
            // so it can be referenced by name from other containers
            net_config.aliases.push(svc_name.to_string());

            config.networks.insert(net_id, net_config);
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_uuid() -> Uuid {
        Uuid::from("test-app-uuid")
    }

    #[test]
    fn preserves_explicit_config_fields_using_label_config_fields() {
        let original = oci::ContainerConfig {
            command: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            cgroup: oci::Cgroup::Host,
            cgroup_parent: Some("/custom".to_string()),
            cpuset: Some("0-3".to_string()),
            cpu_rt_period: 1_000_000,
            cpu_rt_runtime: 950_000,
            cpu_shares: 2048,
            domainname: Some("example.com".to_string()),
            hostname: Some("my-host".to_string()),
            init: Some(true),
            mem_limit: 1073741824,
            mem_reservation: 536870912,
            nano_cpus: 1_500_000_000,
            oom_score_adj: Some(-500),
            pids_limit: Some(100),
            runtime: Some("runc".to_string()),
            shm_size: Some(67108864),
            stop_grace_period: Some(30),
            stop_signal: Some("SIGTERM".to_string()),
            user: Some("1000:1000".to_string()),
            userns_mode: Some("host".to_string()),
            uts: Some("host".to_string()),
            working_dir: Some("/app".to_string()),
            ..Default::default()
        };
        let svc = ServiceConfig(original.clone());
        let with_labels = svc.into_oci_config(1, "svc", &make_uuid());

        let back = ServiceConfig::from(with_labels);
        assert_eq!(back.command, original.command);
        assert_eq!(back.cgroup, original.cgroup);
        assert_eq!(back.cgroup_parent, original.cgroup_parent);
        assert_eq!(back.cpuset, original.cpuset);
        assert_eq!(back.cpu_rt_period, original.cpu_rt_period);
        assert_eq!(back.cpu_rt_runtime, original.cpu_rt_runtime);
        assert_eq!(back.cpu_shares, original.cpu_shares);
        assert_eq!(back.domainname, original.domainname);
        assert_eq!(back.hostname, original.hostname);
        assert_eq!(back.init, original.init);
        assert_eq!(back.mem_limit, original.mem_limit);
        assert_eq!(back.mem_reservation, original.mem_reservation);
        assert_eq!(back.nano_cpus, original.nano_cpus);
        assert_eq!(back.oom_score_adj, original.oom_score_adj);
        assert_eq!(back.pids_limit, original.pids_limit);
        assert_eq!(back.runtime, original.runtime);
        assert_eq!(back.shm_size, original.shm_size);
        assert_eq!(back.stop_grace_period, original.stop_grace_period);
        assert_eq!(back.stop_signal, original.stop_signal);
        assert_eq!(back.user, original.user);
        assert_eq!(back.userns_mode, original.userns_mode);
        assert_eq!(back.uts, original.uts);
        assert_eq!(back.working_dir, original.working_dir);
    }

    #[test]
    fn drops_config_fields_when_not_in_label_config_fields() {
        // Simulate an inspect where the engine/image filled in values the
        // composition never requested. Fields that default to ""/0 are
        // filtered in helios-oci during inspect.
        let labels = HashMap::from([(LABEL_CONFIG_FIELDS.to_string(), "[]".to_string())]);
        let inspected = oci::ContainerConfig {
            command: Some(vec!["/bin/sh".to_string()]),
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
}
