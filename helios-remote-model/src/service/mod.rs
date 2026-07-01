use std::collections::HashMap;

use crate::duration::{DurationMicros, DurationSecs};
use serde::{Deserialize, Deserializer};

use crate::byte_size::ByteSize;
use crate::common_types::{Environment, ImageUri, Value};

mod cgroup;
mod command;
mod healthcheck;
mod network_mode;
mod networks;
mod ports;
mod restart_policy;
mod volumes;

pub use cgroup::*;
pub use command::*;
pub use healthcheck::*;
pub use network_mode::*;
pub use networks::*;
pub use ports::*;
pub use restart_policy::*;
pub use volumes::*;

use super::labels::Labels;

/// Target service as defined by the remote backend
#[derive(Deserialize, Debug)]
pub struct Service {
    pub id: u32,
    pub image: ImageUri,

    #[serde(default)]
    pub labels: HashMap<String, String>,

    #[serde(default)]
    pub environment: HashMap<String, Option<Value>>,

    #[serde(default)]
    pub composition: ServiceComposition,
}

// FIXME: add remaining fields
#[derive(Deserialize, Debug, Default)]
pub struct ServiceComposition {
    /// Linux capabilities to add to the container.
    #[serde(default)]
    pub cap_add: Option<Vec<String>>,

    /// Linux capabilities to drop from the container.
    #[serde(default)]
    pub cap_drop: Option<Vec<String>>,

    #[serde(default)]
    pub cgroup: Option<Cgroup>,

    #[serde(default)]
    pub cgroup_parent: Option<String>,

    #[serde(default)]
    pub command: Option<Command>,

    #[serde(default)]
    pub cpuset: Option<String>,

    #[serde(default)]
    pub cpu_rt_period: Option<DurationMicros>,

    #[serde(default)]
    pub cpu_rt_runtime: Option<DurationMicros>,

    #[serde(default)]
    pub cpu_shares: Option<i64>,

    /// Fractional CPU count such as `1.5` (Compose `cpus`). Rejected at
    /// deserialization time if non-finite, negative, or large enough to
    /// overflow `i64` when converted to nano_cpus (`* 1_000_000_000`), since
    /// the engine takes nano_cpus and would otherwise saturate silently.
    #[serde(default, deserialize_with = "deserialize_cpus")]
    pub cpus: Option<f64>,

    /// Custom DNS servers, single string or list.
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    pub dns: Option<Vec<String>>,

    /// Custom DNS resolver options.
    #[serde(default)]
    pub dns_opt: Option<Vec<String>>,

    /// Custom DNS search domains, single string or list.
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    pub dns_search: Option<Vec<String>>,

    #[serde(default)]
    pub domainname: Option<String>,

    #[serde(default)]
    pub entrypoint: Option<Command>,

    #[serde(default)]
    pub environment: Environment,

    #[serde(default)]
    pub hostname: Option<String>,

    /// Whether to run an init process inside the container. Modeled as
    /// `Option<bool>` rather than `bool` to distinguish user-set from
    /// daemon/Podman global default (`--init` for dockerd or `init=true`
    /// for Podman containers.conf).
    #[serde(default)]
    pub init: Option<bool>,

    #[serde(default)]
    pub labels: Labels,

    #[serde(default)]
    pub mem_limit: Option<ByteSize>,

    #[serde(default)]
    pub mem_reservation: Option<ByteSize>,

    #[serde(default)]
    pub privileged: bool,

    #[serde(default)]
    pub read_only: bool,

    /// Container security options. Only `no-new-privileges`,
    /// `apparmor=unconfined` and `seccomp=unconfined` are permitted.
    #[serde(default, deserialize_with = "deserialize_security_opt")]
    pub security_opt: Option<Vec<String>>,

    #[serde(default)]
    pub restart: RestartPolicy,

    #[serde(default)]
    pub runtime: Option<String>,

    #[serde(default)]
    pub shm_size: Option<ByteSize>,

    #[serde(default)]
    pub stop_grace_period: Option<DurationSecs>,

    #[serde(default)]
    pub stop_signal: Option<String>,

    #[serde(default)]
    pub tty: bool,

    #[serde(default)]
    pub user: Option<String>,

    #[serde(default)]
    pub userns_mode: Option<String>,

    /// UTS namespace mode (`host` or empty)
    #[serde(default)]
    pub uts: Option<String>,

    #[serde(default)]
    pub working_dir: Option<String>,

    #[serde(default)]
    pub networks: NetworkingConfig,

    #[serde(default)]
    pub oom_score_adj: Option<i64>,

    /// Extra `/etc/hosts` entries as a hostname -> IP map.
    #[serde(default, deserialize_with = "deserialize_extra_hosts")]
    pub extra_hosts: Option<HashMap<String, String>>,

    /// Kernel parameters (sysctls) to set in the container. Only kernel-namespaced values are permitted.
    #[serde(default, deserialize_with = "deserialize_sysctls")]
    pub sysctls: Option<HashMap<String, String>>,

    #[serde(default)]
    pub pids_limit: Option<i64>,

    #[serde(default)]
    pub network_mode: Option<NetworkMode>,

    #[serde(default)]
    pub ports: Ports,

    #[serde(default)]
    pub volumes: VolumesConfig,

    #[serde(default)]
    pub healthcheck: Option<Healthcheck>,
}

fn validate_cpus(cpus: f64) -> Result<f64, String> {
    if !cpus.is_finite() || cpus < 0.0 {
        return Err(format!(
            "`cpus` must be a finite non-negative number, got {cpus}"
        ));
    }
    // Engine takes nano_cpus (i64). Round to nearest to avoid drift on values
    // like `0.3` whose binary f64 representation is slightly below the rational.
    if (cpus * 1_000_000_000.0).round() > i64::MAX as f64 {
        return Err(format!("`cpus` value {cpus} overflows i64 nano_cpus"));
    }
    Ok(cpus)
}

fn deserialize_cpus<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<f64>::deserialize(deserializer)?
        .map(validate_cpus)
        .transpose()
        .map_err(serde::de::Error::custom)
}

/// Validate a single `security_opt` entry against the permitted allowlist,
/// replacing `:` for `=` to match what the docker engine expects internally.
fn normalize_security_opt(opt: String) -> Result<String, String> {
    let normalized = opt.replacen(':', "=", 1);
    match normalized.as_str() {
        "no-new-privileges"
        | "no-new-privileges=true"
        | "apparmor=unconfined"
        | "seccomp=unconfined" => Ok(normalized),
        _ => Err(format!(
            "only `no-new-privileges`, `apparmor=unconfined` and `seccomp=unconfined` are allowed, got `{opt}`"
        )),
    }
}

fn deserialize_security_opt<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Vec<String>>::deserialize(deserializer)?
        .map(|opts| {
            opts.into_iter()
                .map(normalize_security_opt)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(serde::de::Error::custom)
}

/// Deserialize a Compose `string_or_list` value (such as `dns` or
/// `dns_search`) into a list, wrapping a single string in a one-element list.
fn deserialize_string_or_list<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrList {
        String(String),
        List(Vec<String>),
    }

    Ok(
        Option::<StringOrList>::deserialize(deserializer)?.map(|raw| match raw {
            StringOrList::String(s) => vec![s],
            StringOrList::List(list) => list,
        }),
    )
}

/// Deserialize Compose `extra_hosts` from a list of `host:ip`/`host=ip`
/// strings or a `host: ip` mapping into a hostname -> IP map.
fn deserialize_extra_hosts<'de, D>(
    deserializer: D,
) -> Result<Option<HashMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ExtraHosts {
        List(Vec<String>),
        Map(HashMap<String, String>),
    }

    let Some(raw) = Option::<ExtraHosts>::deserialize(deserializer)? else {
        return Ok(None);
    };

    let map = match raw {
        ExtraHosts::Map(map) => map,
        ExtraHosts::List(entries) => entries
            .into_iter()
            .map(|entry| {
                // The hostname can't contain `:` or `=`, so the first of either
                // separates host from IP
                let sep = entry.find([':', '=']).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "entry `{entry}` must be in `host:ip` or `host=ip` form"
                    ))
                })?;
                Ok((entry[..sep].to_string(), entry[sep + 1..].to_string()))
            })
            .collect::<Result<_, D::Error>>()?,
    };

    Ok(Some(map))
}

/// Validate kernel namespaces.
///
/// See https://docs.docker.com/reference/cli/docker/container/run/#sysctl
fn is_namespaced_sysctl(key: &str) -> bool {
    key.starts_with("net.")
        || key.starts_with("fs.mqueue.")
        || key.starts_with("kernel.shm")
        || key.starts_with("kernel.msg")
        || key == "kernel.sem"
}

/// Deserialize Compose `sysctls` from a list of `key=value` strings or a
/// `key: value` mapping into a parameter -> value map. Numeric map values are
/// accepted and coerced to strings. Only kernel-namespaced keys are allowed.
fn deserialize_sysctls<'de, D>(deserializer: D) -> Result<Option<HashMap<String, String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ScalarValue {
        Int(i64),
        Float(f64),
        Bool(bool),
        String(String),
    }

    impl std::fmt::Display for ScalarValue {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ScalarValue::String(s) => write!(f, "{s}"),
                ScalarValue::Int(i) => write!(f, "{i}"),
                ScalarValue::Float(n) => write!(f, "{n}"),
                ScalarValue::Bool(b) => write!(f, "{b}"),
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Sysctls {
        List(Vec<String>),
        Map(HashMap<String, ScalarValue>),
    }

    let Some(raw) = Option::<Sysctls>::deserialize(deserializer)? else {
        return Ok(None);
    };

    let map: HashMap<String, String> = match raw {
        Sysctls::Map(map) => map
            .into_iter()
            .map(|(key, value)| (key, value.to_string()))
            .collect(),
        Sysctls::List(entries) => entries
            .into_iter()
            .map(|entry| {
                let (key, value) = entry.split_once('=').ok_or_else(|| {
                    serde::de::Error::custom(format!("entry `{entry}` must be in `key=value` form"))
                })?;
                Ok((key.to_string(), value.to_string()))
            })
            .collect::<Result<_, D::Error>>()?,
    };

    for key in map.keys() {
        if !is_namespaced_sysctl(key) {
            return Err(serde::de::Error::custom(format!(
                "only kernel-namespaced sysctls (`net.*`, `fs.mqueue.*`, `kernel.shm*`, `kernel.msg*`, `kernel.sem`) are allowed, got `{key}`"
            )));
        }
    }

    Ok(Some(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_defaults_restart_to_always() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.restart, RestartPolicy::Always);
    }

    #[test]
    fn composition_parses_stop_grace_period_string() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"stop_grace_period": "15s"})).unwrap();
        assert_eq!(comp.stop_grace_period.map(DurationSecs::to_i64), Some(15));
    }

    #[test]
    fn composition_parses_stop_grace_period_int() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"stop_grace_period": 30})).unwrap();
        assert_eq!(comp.stop_grace_period.map(DurationSecs::to_i64), Some(30));
    }

    #[test]
    fn composition_parses_entrypoint_string() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"entrypoint": "/bin/sh -c"})).unwrap();
        assert_eq!(
            comp.entrypoint.as_deref(),
            Some(&vec!["/bin/sh".to_string(), "-c".to_string()])
        );
    }

    #[test]
    fn composition_parses_entrypoint_list() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"entrypoint": ["/bin/sh", "-c"]})).unwrap();
        assert_eq!(
            comp.entrypoint.as_deref(),
            Some(&vec!["/bin/sh".to_string(), "-c".to_string()])
        );
    }

    #[test]
    fn composition_with_restart_policy() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"restart": "on-failure:5"})).unwrap();
        assert_eq!(comp.restart, RestartPolicy::OnFailure { max_retries: 5 });
    }

    #[test]
    fn service_environment_parses_string_bool() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "DEBUG": "true", "VERBOSE": "false" }
        }))
        .unwrap();
        assert_eq!(svc.environment.get("DEBUG"), Some(&Some(Value::Bool(true))));
        assert_eq!(
            svc.environment.get("VERBOSE"),
            Some(&Some(Value::Bool(false)))
        );
    }

    #[test]
    fn service_environment_parses_string_numbers() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "PORT": "8080", "OFFSET": "-5", "RATIO": "1.5" }
        }))
        .unwrap();
        assert_eq!(
            svc.environment.get("PORT"),
            Some(&Some(Value::Unsigned(8080)))
        );
        assert_eq!(
            svc.environment.get("OFFSET"),
            Some(&Some(Value::Signed(-5)))
        );
        assert_eq!(svc.environment.get("RATIO"), Some(&Some(Value::Float(1.5))));
    }

    #[test]
    fn service_environment_preserves_native_types() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "FLAG": true, "COUNT": 42, "RATIO": 5.14 }
        }))
        .unwrap();
        assert_eq!(svc.environment.get("FLAG"), Some(&Some(Value::Bool(true))));
        assert_eq!(
            svc.environment.get("COUNT"),
            Some(&Some(Value::Unsigned(42)))
        );
        assert_eq!(
            svc.environment.get("RATIO"),
            Some(&Some(Value::Float(5.14)))
        );
    }

    #[test]
    fn composition_bool_fields_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!comp.privileged);
        assert!(!comp.read_only);
        assert!(!comp.tty);
        assert_eq!(comp.init, None);
    }

    #[test]
    fn composition_bool_fields_parse_explicit_values() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "privileged": true,
            "read_only": true,
            "tty": true,
            "init": true,
        }))
        .unwrap();
        assert!(comp.privileged);
        assert!(comp.read_only);
        assert!(comp.tty);
        assert_eq!(comp.init, Some(true));
    }

    #[test]
    fn composition_string_fields_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.cgroup, None);
        assert_eq!(comp.cgroup_parent, None);
        assert_eq!(comp.cpuset, None);
        assert_eq!(comp.domainname, None);
        assert_eq!(comp.hostname, None);
        assert_eq!(comp.runtime, None);
        assert_eq!(comp.stop_signal, None);
        assert_eq!(comp.user, None);
        assert_eq!(comp.userns_mode, None);
        assert_eq!(comp.uts, None);
        assert_eq!(comp.working_dir, None);
    }

    #[test]
    fn composition_string_fields_parse_explicit_values() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "cgroup": "host",
            "cgroup_parent": "/custom",
            "cpuset": "0-3",
            "domainname": "example.com",
            "hostname": "my-host",
            "runtime": "runc",
            "stop_signal": "SIGTERM",
            "user": "1000:1000",
            "userns_mode": "host",
            "uts": "host",
            "working_dir": "/app",
        }))
        .unwrap();
        assert_eq!(comp.cgroup, Some(Cgroup::Host));
        assert_eq!(comp.cgroup_parent.as_deref(), Some("/custom"));
        assert_eq!(comp.cpuset.as_deref(), Some("0-3"));
        assert_eq!(comp.domainname.as_deref(), Some("example.com"));
        assert_eq!(comp.hostname.as_deref(), Some("my-host"));
        assert_eq!(comp.runtime.as_deref(), Some("runc"));
        assert_eq!(comp.stop_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(comp.user.as_deref(), Some("1000:1000"));
        assert_eq!(comp.userns_mode.as_deref(), Some("host"));
        assert_eq!(comp.uts.as_deref(), Some("host"));
        assert_eq!(comp.working_dir.as_deref(), Some("/app"));
    }

    #[test]
    fn composition_number_fields_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.cpu_rt_period, None);
        assert_eq!(comp.cpu_rt_runtime, None);
        assert_eq!(comp.cpu_shares, None);
        assert_eq!(comp.cpus, None);
        assert_eq!(comp.mem_limit, None);
        assert_eq!(comp.mem_reservation, None);
        assert_eq!(comp.oom_score_adj, None);
        assert_eq!(comp.pids_limit, None);
        assert_eq!(comp.shm_size, None);
        assert_eq!(comp.stop_grace_period, None);
    }

    #[test]
    fn composition_number_fields_parse_explicit_values() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "cpu_rt_period": 1000000,
            "cpu_rt_runtime": 950000,
            "cpu_shares": 2048,
            "cpus": 1.5,
            "mem_limit": 1073741824i64,
            "mem_reservation": 536870912i64,
            "oom_score_adj": -500,
            "pids_limit": 100,
            "shm_size": 67108864,
            "stop_grace_period": 30,
        }))
        .unwrap();
        assert_eq!(
            comp.cpu_rt_period.map(DurationMicros::to_i64),
            Some(1000000)
        );
        assert_eq!(
            comp.cpu_rt_runtime.map(DurationMicros::to_i64),
            Some(950000)
        );
        assert_eq!(comp.cpu_shares, Some(2048));
        assert_eq!(comp.cpus, Some(1.5));
        assert_eq!(comp.mem_limit.map(ByteSize::to_bytes), Some(1073741824));
        assert_eq!(
            comp.mem_reservation.map(ByteSize::to_bytes),
            Some(536870912)
        );
        assert_eq!(comp.oom_score_adj, Some(-500));
        assert_eq!(comp.pids_limit, Some(100));
        assert_eq!(comp.shm_size.map(ByteSize::to_bytes), Some(67108864));
        assert_eq!(comp.stop_grace_period.map(DurationSecs::to_i64), Some(30));
    }

    #[test]
    fn composition_number_fields_parse_cpu_rt_duration_strings() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "cpu_rt_period": "1000000us",
            "cpu_rt_runtime": "1s",
        }))
        .unwrap();
        assert_eq!(
            comp.cpu_rt_period.map(DurationMicros::to_i64),
            Some(1000000)
        );
        assert_eq!(
            comp.cpu_rt_runtime.map(DurationMicros::to_i64),
            Some(1000000)
        );

        let comp2: ServiceComposition = serde_json::from_value(serde_json::json!({
            "cpu_rt_period": "900ms",
            "cpu_rt_runtime": "200000ns",
        }))
        .unwrap();
        assert_eq!(
            comp2.cpu_rt_period.map(DurationMicros::to_i64),
            Some(900000)
        );
        assert_eq!(comp2.cpu_rt_runtime.map(DurationMicros::to_i64), Some(200));
    }

    #[test]
    fn composition_byte_fields_parse_strings() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "mem_limit": "1g",
            "mem_reservation": "512mb",
            "shm_size": "64MiB",
        }))
        .unwrap();
        assert_eq!(comp.mem_limit.map(ByteSize::to_bytes), Some(1073741824));
        assert_eq!(
            comp.mem_reservation.map(ByteSize::to_bytes),
            Some(536870912)
        );
        assert_eq!(comp.shm_size.map(ByteSize::to_bytes), Some(67108864));
    }

    #[test]
    fn composition_ports_default_empty() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(comp.ports.is_empty());
    }

    #[test]
    fn composition_ports_accept_mixed_forms() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "ports": [8080, "127.0.0.1:5353:53/udp", { "target": 443, "published": 8443 }],
        }))
        .unwrap();
        let ports: Vec<&PortMapping> = comp.ports.iter().collect();
        assert_eq!(
            ports,
            vec![
                &PortMapping {
                    target: 8080,
                    published: None,
                    host_ip: None,
                    protocol: PortProtocol::Tcp,
                },
                &PortMapping {
                    target: 53,
                    published: Some(HostPort::Single(5353)),
                    host_ip: Some("127.0.0.1".to_string()),
                    protocol: PortProtocol::Udp,
                },
                &PortMapping {
                    target: 443,
                    published: Some(HostPort::Single(8443)),
                    host_ip: None,
                    protocol: PortProtocol::Tcp,
                },
            ]
        );
    }

    #[test]
    fn composition_extra_hosts_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.extra_hosts, None);
    }

    #[test]
    fn composition_extra_hosts_accepts_list_with_either_separator() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "extra_hosts": ["foo=127.0.0.1", "bar:8.8.8.8", "v6:2001:db8::1"],
        }))
        .unwrap();
        assert_eq!(
            comp.extra_hosts,
            Some(HashMap::from([
                ("foo".to_string(), "127.0.0.1".to_string()),
                ("bar".to_string(), "8.8.8.8".to_string()),
                ("v6".to_string(), "2001:db8::1".to_string()),
            ]))
        );
    }

    #[test]
    fn composition_extra_hosts_accepts_mapping() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "extra_hosts": {"foo": "127.0.0.1", "bar": "8.8.8.8"},
        }))
        .unwrap();
        assert_eq!(
            comp.extra_hosts,
            Some(HashMap::from([
                ("foo".to_string(), "127.0.0.1".to_string()),
                ("bar".to_string(), "8.8.8.8".to_string()),
            ]))
        );
    }

    #[test]
    fn composition_extra_hosts_rejects_entry_without_separator() {
        let err = serde_json::from_value::<ServiceComposition>(serde_json::json!({
            "extra_hosts": ["noseparator"],
        }))
        .unwrap_err();
        assert!(err.to_string().contains("host:ip"), "{err}");
    }

    #[test]
    fn composition_sysctls_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.sysctls, None);
    }

    #[test]
    fn composition_sysctls_accepts_list() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "sysctls": ["net.core.somaxconn=1024", "net.ipv4.tcp_syncookies=0"],
        }))
        .unwrap();
        assert_eq!(
            comp.sysctls,
            Some(HashMap::from([
                ("net.core.somaxconn".to_string(), "1024".to_string()),
                ("net.ipv4.tcp_syncookies".to_string(), "0".to_string()),
            ]))
        );
    }

    #[test]
    fn composition_sysctls_accepts_mapping_and_coerces_numbers() {
        // Map values may arrive as numbers; they are coerced to strings.
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "sysctls": {"net.core.somaxconn": 1024, "net.ipv4.tcp_syncookies": "0"},
        }))
        .unwrap();
        assert_eq!(
            comp.sysctls,
            Some(HashMap::from([
                ("net.core.somaxconn".to_string(), "1024".to_string()),
                ("net.ipv4.tcp_syncookies".to_string(), "0".to_string()),
            ]))
        );
    }

    #[test]
    fn composition_sysctls_rejects_non_namespaced() {
        // Both list and map forms reject keys outside the namespaced allowlist.
        let err = serde_json::from_value::<ServiceComposition>(serde_json::json!({
            "sysctls": ["kernel.hostname=evil"],
        }))
        .unwrap_err();
        assert!(err.to_string().contains("kernel-namespaced"), "{err}");

        let err = serde_json::from_value::<ServiceComposition>(serde_json::json!({
            "sysctls": {"vm.swappiness": "10"},
        }))
        .unwrap_err();
        assert!(err.to_string().contains("kernel-namespaced"), "{err}");
    }

    #[test]
    fn composition_sysctls_rejects_list_entry_without_separator() {
        let err = serde_json::from_value::<ServiceComposition>(serde_json::json!({
            "sysctls": ["net.core.somaxconn"],
        }))
        .unwrap_err();
        assert!(err.to_string().contains("key=value"), "{err}");
    }

    #[test]
    fn composition_dns_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.dns, None);
        assert_eq!(comp.dns_opt, None);
        assert_eq!(comp.dns_search, None);
    }

    #[test]
    fn composition_dns_accepts_single_string() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "dns": "8.8.8.8",
            "dns_search": "example.com",
        }))
        .unwrap();
        assert_eq!(comp.dns, Some(vec!["8.8.8.8".to_string()]));
        assert_eq!(comp.dns_search, Some(vec!["example.com".to_string()]));
    }

    #[test]
    fn composition_dns_accepts_list() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "dns": ["8.8.8.8", "9.9.9.9"],
            "dns_opt": ["use-vc", "no-tld-query"],
            "dns_search": ["dc1.example.com", "dc2.example.com"],
        }))
        .unwrap();
        assert_eq!(
            comp.dns,
            Some(vec!["8.8.8.8".to_string(), "9.9.9.9".to_string()])
        );
        assert_eq!(
            comp.dns_opt,
            Some(vec!["use-vc".to_string(), "no-tld-query".to_string()])
        );
        assert_eq!(
            comp.dns_search,
            Some(vec![
                "dc1.example.com".to_string(),
                "dc2.example.com".to_string()
            ])
        );
    }

    #[test]
    fn composition_cap_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.cap_add, None);
        assert_eq!(comp.cap_drop, None);
    }

    #[test]
    fn composition_cap_accepts_list() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "cap_add": ["ALL"],
            "cap_drop": ["NET_ADMIN", "SYS_ADMIN"],
        }))
        .unwrap();
        assert_eq!(comp.cap_add, Some(vec!["ALL".to_string()]));
        assert_eq!(
            comp.cap_drop,
            Some(vec!["NET_ADMIN".to_string(), "SYS_ADMIN".to_string()])
        );
    }

    #[test]
    fn composition_security_opt_default_unset() {
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(comp.security_opt, None);
    }

    #[test]
    fn composition_security_opt_accepts_allowed_and_normalizes() {
        // `:` and `=` separators are both accepted and stored with `=`.
        let comp: ServiceComposition = serde_json::from_value(serde_json::json!({
            "security_opt": [
                "no-new-privileges",
                "no-new-privileges:true",
                "apparmor:unconfined",
                "seccomp=unconfined",
            ],
        }))
        .unwrap();
        assert_eq!(
            comp.security_opt,
            Some(vec![
                "no-new-privileges".to_string(),
                "no-new-privileges=true".to_string(),
                "apparmor=unconfined".to_string(),
                "seccomp=unconfined".to_string(),
            ])
        );
    }

    #[test]
    fn composition_security_opt_rejects_disallowed() {
        for value in [
            "label:user:USER",
            "apparmor=custom",
            "seccomp=/profile.json",
            "no-new-privileges=false",
        ] {
            let _ = serde_json::from_value::<ServiceComposition>(serde_json::json!({
                "security_opt": [value],
            }))
            .unwrap_err();
        }
    }

    #[test]
    fn validate_cpus_accepts_finite_non_negative() {
        assert_eq!(validate_cpus(0.0), Ok(0.0));
        assert_eq!(validate_cpus(0.3), Ok(0.3));
        assert_eq!(validate_cpus(1.5), Ok(1.5));
        assert_eq!(validate_cpus(2.0), Ok(2.0));
    }

    #[test]
    fn validate_cpus_rejects_non_finite_or_negative() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5] {
            let err = validate_cpus(bad).unwrap_err();
            assert!(
                err.contains("finite non-negative"),
                "unexpected error for {bad}: {err}"
            );
        }
    }

    #[test]
    fn validate_cpus_rejects_overflow() {
        // 1e10 cpus * 1e9 ns/cpu = 1e19, which exceeds i64::MAX (~9.2e18).
        let err = validate_cpus(1e10).unwrap_err();
        assert!(err.contains("overflows i64 nano_cpus"));
    }

    #[test]
    fn composition_cpus_rejects_invalid_on_deserialize() {
        let err = serde_json::from_value::<ServiceComposition>(serde_json::json!({"cpus": -0.5}))
            .unwrap_err();
        assert!(err.to_string().contains("finite non-negative"));

        let err = serde_json::from_value::<ServiceComposition>(serde_json::json!({"cpus": 1e10}))
            .unwrap_err();
        assert!(err.to_string().contains("overflows i64 nano_cpus"));
    }

    #[test]
    fn composition_init_preserves_explicit_false() {
        let comp: ServiceComposition =
            serde_json::from_value(serde_json::json!({"init": false})).unwrap();
        assert_eq!(comp.init, Some(false));
    }

    #[test]
    fn service_environment_handles_null_and_string() {
        let svc: Service = serde_json::from_value(serde_json::json!({
            "id": 1,
            "image": "registry.example.com/repo/image@sha256:1234567890abcdef1234567890abcdef12345678",
            "environment": { "UNSET": null, "HOST": "localhost" }
        }))
        .unwrap();
        assert_eq!(svc.environment.get("UNSET"), Some(&None));
        assert_eq!(
            svc.environment.get("HOST"),
            Some(&Some(Value::String("localhost".to_string())))
        );
    }
}
