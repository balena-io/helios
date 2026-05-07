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
        let label_config_fields: Vec<String> = labels
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
        if !label_config_fields.contains(&"cgroup".to_string()) {
            config.cgroup = None;
        }
        if !label_config_fields.contains(&"cgroup_parent".to_string()) {
            config.cgroup_parent = None;
        }
        if !label_config_fields.contains(&"command".to_string()) {
            config.command = None;
        }
        if !label_config_fields.contains(&"cpuset".to_string()) {
            config.cpuset = None;
        }
        if !label_config_fields.contains(&"cpu_rt_period".to_string()) {
            config.cpu_rt_period = None;
        }
        if !label_config_fields.contains(&"cpu_rt_runtime".to_string()) {
            config.cpu_rt_runtime = None;
        }
        if !label_config_fields.contains(&"cpu_shares".to_string()) {
            config.cpu_shares = None;
        }
        if !label_config_fields.contains(&"domainname".to_string()) {
            config.domainname = None;
        }
        if !label_config_fields.contains(&"hostname".to_string()) {
            config.hostname = None;
        }
        // The daemon / Podman can set a default value for init
        if !label_config_fields.contains(&"init".to_string()) {
            config.init = None;
        }
        if !label_config_fields.contains(&"mem_limit".to_string()) {
            config.mem_limit = None;
        }
        if !label_config_fields.contains(&"mem_reservation".to_string()) {
            config.mem_reservation = None;
        }
        if !label_config_fields.contains(&"nano_cpus".to_string()) {
            config.nano_cpus = None;
        }
        if !label_config_fields.contains(&"oom_score_adj".to_string()) {
            config.oom_score_adj = None;
        }
        if !label_config_fields.contains(&"pids_limit".to_string()) {
            config.pids_limit = None;
        }
        if !label_config_fields.contains(&"runtime".to_string()) {
            config.runtime = None;
        }
        if !label_config_fields.contains(&"shm_size".to_string()) {
            config.shm_size = None;
        }
        if !label_config_fields.contains(&"stop_grace_period".to_string()) {
            config.stop_grace_period = None;
        }
        if !label_config_fields.contains(&"stop_signal".to_string()) {
            config.stop_signal = None;
        }
        if !label_config_fields.contains(&"user".to_string()) {
            config.user = None;
        }
        if !label_config_fields.contains(&"userns_mode".to_string()) {
            config.userns_mode = None;
        }
        if !label_config_fields.contains(&"uts".to_string()) {
            config.uts = None;
        }
        if !label_config_fields.contains(&"working_dir".to_string()) {
            config.working_dir = None;
        }

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

        // List of config fields coming from the composition. This is only necessary for fields that
        // may be shared between the image and service, since docker will use the image version as
        // the default unless overriden
        let mut fields = Vec::new();

        if config.cgroup.is_some() {
            fields.push("cgroup");
        }
        if config.cgroup_parent.is_some() {
            fields.push("cgroup_parent");
        }
        if config.command.is_some() {
            fields.push("command");
        }
        if config.cpuset.is_some() {
            fields.push("cpuset");
        }
        if config.cpu_rt_period.is_some() {
            fields.push("cpu_rt_period");
        }
        if config.cpu_rt_runtime.is_some() {
            fields.push("cpu_rt_runtime");
        }
        if config.cpu_shares.is_some() {
            fields.push("cpu_shares");
        }
        if config.domainname.is_some() {
            fields.push("domainname");
        }
        if config.hostname.is_some() {
            fields.push("hostname");
        }
        if config.init.is_some() {
            fields.push("init");
        }
        if config.mem_limit.is_some() {
            fields.push("mem_limit");
        }
        if config.mem_reservation.is_some() {
            fields.push("mem_reservation");
        }
        if config.nano_cpus.is_some() {
            fields.push("nano_cpus");
        }
        if config.oom_score_adj.is_some() {
            fields.push("oom_score_adj");
        }
        if config.pids_limit.is_some() {
            fields.push("pids_limit");
        }
        if config.runtime.is_some() {
            fields.push("runtime");
        }
        if config.shm_size.is_some() {
            fields.push("shm_size");
        }
        if config.stop_grace_period.is_some() {
            fields.push("stop_grace_period");
        }
        if config.stop_signal.is_some() {
            fields.push("stop_signal");
        }
        if config.user.is_some() {
            fields.push("user");
        }
        if config.userns_mode.is_some() {
            fields.push("userns_mode");
        }
        if config.uts.is_some() {
            fields.push("uts");
        }
        if config.working_dir.is_some() {
            fields.push("working_dir");
        }

        // Create a label LABEL_CONFIG_FIELDS label with all the custom fields
        let label_config_fields_value = fields
            .into_iter()
            .map(|s| json::Value::String(s.to_owned()))
            .collect::<json::Value>();

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
            cgroup: Some(oci::Cgroup::Host),
            cgroup_parent: Some("/custom".to_string()),
            cpuset: Some("0-3".to_string()),
            cpu_rt_period: Some(1_000_000),
            cpu_rt_runtime: Some(950_000),
            cpu_shares: Some(2048),
            domainname: Some("example.com".to_string()),
            hostname: Some("my-host".to_string()),
            init: Some(true),
            mem_limit: Some(1073741824),
            mem_reservation: Some(536870912),
            nano_cpus: Some(1_500_000_000),
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
        // composition never requested.
        let labels = HashMap::from([(LABEL_CONFIG_FIELDS.to_string(), "[]".to_string())]);
        let inspected = oci::ContainerConfig {
            command: Some(vec!["/bin/sh".to_string()]),
            cgroup: Some(oci::Cgroup::Host),
            cgroup_parent: Some("/docker".to_string()),
            cpuset: Some("0-3".to_string()),
            cpu_rt_period: Some(0),
            cpu_rt_runtime: Some(0),
            cpu_shares: Some(1024),
            domainname: Some("auto.local".to_string()),
            hostname: Some("a1b2c3d4e5f6".to_string()),
            init: Some(true),
            mem_limit: Some(0),
            mem_reservation: Some(0),
            nano_cpus: Some(0),
            oom_score_adj: Some(0),
            pids_limit: Some(0),
            runtime: Some("runc".to_string()),
            shm_size: Some(67108864),
            stop_grace_period: Some(10),
            stop_signal: Some("SIGTERM".to_string()),
            user: Some("root".to_string()),
            userns_mode: Some("host".to_string()),
            uts: Some("host".to_string()),
            working_dir: Some("/".to_string()),
            labels,
            ..Default::default()
        };
        let svc = ServiceConfig::from(inspected);
        assert_eq!(svc.command, None);
        assert_eq!(svc.cgroup, None);
        assert_eq!(svc.cgroup_parent, None);
        assert_eq!(svc.cpuset, None);
        assert_eq!(svc.cpu_rt_period, None);
        assert_eq!(svc.cpu_rt_runtime, None);
        assert_eq!(svc.cpu_shares, None);
        assert_eq!(svc.domainname, None);
        assert_eq!(svc.hostname, None);
        assert_eq!(svc.init, None);
        assert_eq!(svc.mem_limit, None);
        assert_eq!(svc.mem_reservation, None);
        assert_eq!(svc.nano_cpus, None);
        assert_eq!(svc.oom_score_adj, None);
        assert_eq!(svc.pids_limit, None);
        assert_eq!(svc.runtime, None);
        assert_eq!(svc.shm_size, None);
        assert_eq!(svc.stop_grace_period, None);
        assert_eq!(svc.stop_signal, None);
        assert_eq!(svc.user, None);
        assert_eq!(svc.userns_mode, None);
        assert_eq!(svc.uts, None);
        assert_eq!(svc.working_dir, None);
    }
}
