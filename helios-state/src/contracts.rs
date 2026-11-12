//! The device contract, describing what the device provides to the services it runs

use std::sync::LazyLock;

use contrato::{Contract, Universe};
use mahler::state::{Map, State};
use nix::sys::utsname::uname;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use crate::common_types::{OperatingSystem, Uuid};
use crate::models::AppTarget;
use crate::remote_model::ServiceContract as RemoteServiceContract;
use crate::util::fs::run_async;

/// A service contract, wrapped to make it usable on a state model
///
/// An optional service that does not satisfy the contract is removed from the target instead
/// of causing its release to be rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceContract {
    #[serde(default)]
    pub optional: bool,
    pub contract: Contract,
}

impl From<RemoteServiceContract> for ServiceContract {
    fn from(RemoteServiceContract { optional, contract }: RemoteServiceContract) -> Self {
        Self { optional, contract }
    }
}

impl State for ServiceContract {
    type Target = Self;
}

/// Reject the app releases with services the device contract does not satisfy, and say why
///
/// A reason is given for every unmet requirement.
///
/// A rejected app is left with no releases and a `rejected_release`, the same as an app whose
/// target could not be parsed. A requirement on a type the device contract does not describe
/// is not met. Optional services that do not meet the contract are removed from the release
/// with a warning, and do not cause a rejection, unless another service of the release depends
/// on them.
///
/// All service contracts are removed from the target, as they are not needed to plan.
pub(crate) fn reject_unsatisfied_releases(
    apps: &mut Map<Uuid, AppTarget>,
    contract: &Universe,
) -> Vec<String> {
    let mut reasons = Vec::new();

    for (app_uuid, app) in apps.iter_mut() {
        let mut rejected = None;

        for (rel_uuid, rel) in app.releases.iter_mut() {
            let mut removed: Vec<(String, Vec<String>)> = Vec::new();
            for (svc_name, svc) in rel.services.iter_mut() {
                let Some(ServiceContract {
                    optional,
                    contract: svc_contract,
                }) = svc.contract.take()
                else {
                    continue;
                };
                let unmet = contract.get_not_satisfied_child_requirements(&svc_contract, None);
                if unmet.is_empty() {
                    continue;
                }
                let unmet = unmet.iter().map(|req| {
                    format!(
                        "unmet requirement for service {svc_name} of app {app_uuid}: {}",
                        serde_json::to_string(req).unwrap_or_default()
                    )
                });
                if optional {
                    removed.push((svc_name.clone(), unmet.collect()));
                } else {
                    reasons.extend(unmet);
                    rejected.get_or_insert_with(|| rel_uuid.clone());
                }
            }

            // a service that is removed too does not need what it depends on
            for (svc_name, unmet) in &removed {
                let dependants: Vec<_> = rel
                    .services
                    .iter()
                    .filter(|(name, svc)| {
                        svc.depends_on.contains_key(svc_name)
                            && !removed.iter().any(|(r, _)| r == *name)
                    })
                    .map(|(name, _)| name.as_str())
                    .collect();
                if dependants.is_empty() {
                    for reason in unmet {
                        warn!("removing optional service: {reason}");
                    }
                } else {
                    reasons.extend(unmet.iter().cloned());
                    reasons.push(format!(
                        "optional service {svc_name} of app {app_uuid} has unmet requirements but is required by: {}",
                        dependants.join(", ")
                    ));
                    rejected.get_or_insert_with(|| rel_uuid.clone());
                }
            }
            for (svc_name, _) in removed {
                rel.services.remove(&svc_name);
            }
        }

        if let Some(rel_uuid) = rejected {
            app.releases.clear();
            app.rejected_release = Some(rel_uuid);
        }
    }

    reasons
}

/// The running kernel, as reported by `uname`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Kernel {
    /// The kernel name, as given by `uname -s`
    pub(crate) name: String,
    /// The kernel release, as given by `uname -r`
    pub(crate) release: String,
    /// The hardware identifier, as given by `uname -m`
    pub(crate) machine: String,
}

impl Kernel {
    /// Read the running kernel, if it can be queried
    pub(crate) async fn read() -> Option<Self> {
        let res = run_async(|| {
            let info = uname()?;
            Ok(Self {
                name: info.sysname().to_string_lossy().into_owned(),
                release: info.release().to_string_lossy().into_owned(),
                machine: info.machine().to_string_lossy().into_owned(),
            })
        })
        .await;
        match res {
            Ok(kernel) => Some(kernel),
            Err(e) => {
                warn!("could not query the kernel with uname: {e}");
                None
            }
        }
    }
}

/// Build the device contract from what is known about the device
///
/// Without a known OS, or one that cannot be expressed in a contract (e.g.
/// the name is not a valid slug), the contract has no `sw.os`. Likewise, the
/// contract has no `sw.kernel` without a known kernel or one with an
/// unrecognized version, and no `sw.l4t` or `sw.kernel-bsp` unless the
/// kernel release is a L4T one. It has no `arch.sw` if the machine is not a supported
/// architecture, and no `sw.supervisor` or `sw.application` without a known supervisor version.
pub(crate) fn device_contract(
    os: Option<&OperatingSystem>,
    kernel: Option<&Kernel>,
    supervisor_version: Option<&str>,
) -> Universe {
    let mut universe = Universe::new();

    if let Some(os) = os
        && let Err(e) = os_contract(os).and_then(|contract| universe.add_child(contract).map(drop))
    {
        // the device then fails to satisfy any `sw.os` requirement
        warn!("could not build the `sw.os` contract for '{os}': {e}");
    }

    if let Some(kernel) = kernel {
        for contract in kernel_contracts(kernel)
            .into_iter()
            .chain(arch_contract(kernel))
        {
            if let Err(e) = contract.and_then(|contract| universe.add_child(contract).map(drop)) {
                warn!("could not build a kernel contract for '{kernel:?}': {e}");
            }
        }
    }

    if let Some(version) = supervisor_version
        && let Err(e) = supervisor_contracts(version).and_then(|contracts| {
            contracts
                .into_iter()
                .try_for_each(|contract| universe.add_child(contract).map(drop))
        })
    {
        warn!("could not build the supervisor contracts for '{version}': {e}");
    }

    universe
}

static KERNEL_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+\.\d+\.\d+)").expect("valid regex"));

static L4T_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-l4t-r(\d+\.\d+(\.?\d+)?)").expect("valid regex"));

/// The contracts describing the kernel
///
/// `sw.kernel` has the slugified kernel name as the slug and the `major.minor.patch` part of
/// the release as the version.
///
/// For a Jetson kernel, the `-l4t-rX.Y[.Z]` suffix of the release gives the L4T version, which
/// is described by `sw.kernel-bsp` with the slug `l4t`. `sw.l4t` is kept for contracts
/// written against the legacy supervisor, which require it by version only. A child contract
/// needs a slug, so it gets `l4t` as well.
fn kernel_contracts(
    Kernel { name, release, .. }: &Kernel,
) -> Vec<Result<Contract, contrato::Error>> {
    let mut raw = Vec::new();

    if let Some(version) = KERNEL_VERSION.captures(release).map(|c| c[1].to_string()) {
        raw.push(json!({ "type": "sw.kernel", "slug": slugify(name), "version": version }));
    }

    if let Some(captures) = L4T_VERSION.captures(release) {
        let mut version = captures[1].to_string();
        // a missing patch number is taken as `0`, so versions can always be compared as semver
        if captures.get(2).is_none() {
            version.push_str(".0");
        }
        raw.push(json!({ "type": "sw.l4t", "slug": "l4t", "version": version }));
        raw.push(json!({ "type": "sw.kernel-bsp", "slug": "l4t", "version": version }));
    }

    raw.into_iter().map(Contract::try_from).collect()
}

/// The contracts describing the supervisor, from the version it reports (e.g. `v7.21.0`)
///
/// The supervisor is described by `sw.supervisor` and by `sw.application`, both with the slug
/// `balena-supervisor`, as contracts may require either.
fn supervisor_contracts(version: &str) -> Result<Vec<Contract>, contrato::Error> {
    let version = version.strip_prefix('v').unwrap_or(version);
    ["sw.supervisor", "sw.application"]
        .into_iter()
        .map(|ty| {
            Contract::try_from(
                json!({ "type": ty, "slug": "balena-supervisor", "version": version }),
            )
        })
        .collect()
}

/// The contract describing the device architecture, with the balena architecture as the slug
///
/// Only `amd64`, `aarch64`, `armv7hf`, `rpi` and `i386` are supported. `uname` reports the
/// machine as `x86_64`, `arm64` (or `aarch64` on Linux), `armv7l`, `armv6l` and `i686`
/// (or `i386`) respectively.
fn arch_contract(Kernel { machine, .. }: &Kernel) -> Option<Result<Contract, contrato::Error>> {
    let slug = match machine.as_str() {
        "x86_64" => "amd64",
        "arm64" | "aarch64" => "aarch64",
        "armv7l" => "armv7hf",
        "armv6l" => "rpi",
        "i686" | "i386" => "i386",
        _ => {
            warn!("unsupported architecture '{machine}'");
            return None;
        }
    };
    Some(Contract::try_from(
        json!({ "type": "arch.sw", "slug": slug }),
    ))
}

fn os_contract(os: &OperatingSystem) -> Result<Contract, contrato::Error> {
    let mut raw = json!({
        "type": "sw.os",
        "slug": slugify(&os.name),
    });
    if let Some(version) = &os.version {
        raw["version"] = json!(version);
    }
    Contract::try_from(raw)
}

/// Convert a name into a slug, e.g. `balenaOS` into `balena-os`
///
/// Runs of characters a slug does not allow become a single `-`, which is
/// also trimmed from both ends. The result can still be invalid, e.g. if empty
/// or starting with a digit.
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev: Option<char> = None;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '+' {
            if c.is_ascii_uppercase()
                && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
            {
                slug.push('-');
            }
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        prev = Some(c);
    }
    slug.truncate(slug.trim_end_matches('-').len());
    slug
}

#[cfg(test)]
mod tests {
    use super::*;
    use contrato::Matcher;
    use pretty_assertions::assert_eq;

    fn os(name: &str, version: Option<&str>) -> OperatingSystem {
        OperatingSystem {
            name: name.to_string(),
            version: version.map(str::to_string),
            build: None,
        }
    }

    fn has_os(universe: &Universe) -> bool {
        !universe.find_children(&Matcher::new("sw.os")).is_empty()
    }

    fn service(requires: serde_json::Value) -> Contract {
        Contract::try_from(json!({
            "type": "sw.container",
            "slug": "svc",
            "requires": requires,
        }))
        .unwrap()
    }

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("balenaOS"), "balena-os");
        assert_eq!(slugify("Balena Cloud OS"), "balena-cloud-os");
        assert_eq!(slugify("  Ubuntu  Server "), "ubuntu-server");
        assert_eq!(slugify("debian"), "debian");
    }

    #[test]
    fn builds_the_os_contract() {
        let universe = device_contract(Some(&os("balenaOS", Some("6.5.6+rev1"))), None, None);
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_slug(), Some("balena-os"));
        assert_eq!(found[0].get_version().as_deref(), Some("6.5.6+rev1"));
    }

    #[test]
    fn builds_the_os_contract_without_version() {
        let universe = device_contract(Some(&os("balenaOS", None)), None, None);
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_version(), None);
    }

    #[test]
    fn leaves_the_contract_empty_without_os() {
        let universe = device_contract(None, None, None);
        assert!(universe.find_children(&Matcher::new("sw.os")).is_empty());
    }

    #[test]
    fn omits_the_os_if_the_name_is_empty() {
        assert!(!has_os(&device_contract(
            Some(&os("", Some("1.0.0"))),
            None,
            None
        )));
        assert!(!has_os(&device_contract(
            Some(&os("  ", Some("1.0.0"))),
            None,
            None
        )));
    }

    #[test]
    fn omits_the_os_if_the_name_does_not_start_with_a_letter() {
        assert!(!has_os(&device_contract(
            Some(&os("1OS", Some("1.0.0"))),
            None,
            None
        )));
        assert!(!has_os(&device_contract(
            Some(&os("+os", Some("1.0.0"))),
            None,
            None
        )));
        assert!(!has_os(&device_contract(
            Some(&os("!!!", Some("1.0.0"))),
            None,
            None
        )));
    }

    #[test]
    fn replaces_characters_a_slug_does_not_allow() {
        assert_eq!(slugify("Debian GNU/Linux"), "debian-gnu-linux");
        assert_eq!(slugify("Foo (bar)"), "foo-bar");
        assert_eq!(slugify("balenaOS!"), "balena-os");
        assert_eq!(slugify("_os"), "os");
        assert_eq!(slugify("éOS"), "os");
        assert_eq!(slugify("a - b"), "a-b");

        let universe = device_contract(Some(&os("Debian GNU/Linux", Some("12"))), None, None);
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found[0].get_slug(), Some("debian-gnu-linux"));
    }

    #[test]
    fn accepts_dots_and_plus_in_the_name() {
        let universe = device_contract(Some(&os("os.v2+x", Some("1.0.0"))), None, None);
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found[0].get_slug(), Some("os.v2+x"));
    }

    #[test]
    fn a_version_that_is_not_semver_does_not_fail() {
        let universe = device_contract(Some(&os("balenaOS", Some("wheezy"))), None, None);
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found[0].get_version().as_deref(), Some("wheezy"));
    }

    #[test]
    fn the_contract_satisfies_os_requirements() {
        let universe = device_contract(Some(&os("balenaOS", Some("6.5.6"))), None, None);
        let svc = service(json!([{ "type": "sw.os", "slug": "balena-os", "version": ">=6" }]));
        assert!(universe.satisfies_child_contract(&svc, Some(&["sw.os"])));

        let svc = service(json!([{ "type": "sw.os", "slug": "balena-os", "version": ">=7" }]));
        assert!(!universe.satisfies_child_contract(&svc, Some(&["sw.os"])));
    }

    fn kernel(name: &str, release: &str) -> Kernel {
        Kernel {
            name: name.to_string(),
            release: release.to_string(),
            machine: "x86_64".to_string(),
        }
    }

    #[test]
    fn maps_the_machine_to_the_architecture() {
        for (machine, slug) in [
            ("x86_64", "amd64"),
            ("arm64", "aarch64"),
            ("aarch64", "aarch64"),
            ("armv7l", "armv7hf"),
            ("armv6l", "rpi"),
            ("i686", "i386"),
            ("i386", "i386"),
        ] {
            let kernel = Kernel {
                machine: machine.to_string(),
                ..kernel("Linux", "5.15.148")
            };
            let universe = device_contract(None, Some(&kernel), None);
            let found = universe.find_children(&Matcher::new("arch.sw"));
            assert_eq!(found.len(), 1, "{machine}");
            assert_eq!(found[0].get_slug(), Some(slug), "{machine}");
        }
    }

    #[test]
    fn omits_the_architecture_if_unsupported() {
        let kernel = Kernel {
            machine: "riscv64".to_string(),
            ..kernel("Linux", "5.15.148")
        };
        let universe = device_contract(None, Some(&kernel), None);
        assert!(universe.find_children(&Matcher::new("arch.sw")).is_empty());
    }

    #[test]
    fn the_contract_satisfies_architecture_requirements() {
        let universe = device_contract(None, Some(&kernel("Linux", "5.15.148")), None);
        let svc = service(json!([{ "type": "arch.sw", "slug": "amd64" }]));
        assert!(universe.satisfies_child_contract(&svc, Some(&["arch.sw"])));

        let svc = service(json!([{ "type": "arch.sw", "slug": "rpi" }]));
        assert!(!universe.satisfies_child_contract(&svc, Some(&["arch.sw"])));
    }

    #[test]
    fn builds_the_kernel_contract() {
        let universe = device_contract(
            None,
            Some(&kernel("Linux", "5.15.148-yocto-standard")),
            None,
        );
        let found = universe.find_children(&Matcher::new("sw.kernel"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_slug(), Some("linux"));
        assert_eq!(found[0].get_version().as_deref(), Some("5.15.148"));
        assert!(universe.find_children(&Matcher::new("sw.l4t")).is_empty());
    }

    #[test]
    fn slugifies_the_kernel_name() {
        let universe = device_contract(None, Some(&kernel("Foo Kernel", "1.2.3")), None);
        let found = universe.find_children(&Matcher::new("sw.kernel"));
        assert_eq!(found[0].get_slug(), Some("foo-kernel"));
    }

    #[test]
    fn omits_the_kernel_if_the_version_is_not_recognized() {
        let universe = device_contract(None, Some(&kernel("Linux", "6.1")), None);
        assert!(
            universe
                .find_children(&Matcher::new("sw.kernel"))
                .is_empty()
        );
    }

    #[test]
    fn the_contract_satisfies_kernel_requirements() {
        let universe = device_contract(None, Some(&kernel("Linux", "5.15.148-yocto")), None);
        let svc = service(json!([{ "type": "sw.kernel", "slug": "linux", "version": ">=5.10" }]));
        assert!(universe.satisfies_child_contract(&svc, Some(&["sw.kernel"])));

        let svc = service(json!([{ "type": "sw.kernel", "slug": "linux", "version": ">=6" }]));
        assert!(!universe.satisfies_child_contract(&svc, Some(&["sw.kernel"])));
    }

    #[test]
    fn builds_the_l4t_contracts() {
        let universe = device_contract(
            None,
            Some(&kernel("Linux", "5.15.136-l4t-r36.4.3-tegra")),
            None,
        );
        let found = universe.find_children(&Matcher::new("sw.l4t"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_slug(), Some("l4t"));
        assert_eq!(found[0].get_version().as_deref(), Some("36.4.3"));

        let found = universe.find_children(&Matcher::new("sw.kernel-bsp"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_slug(), Some("l4t"));
        assert_eq!(found[0].get_version().as_deref(), Some("36.4.3"));
    }

    #[test]
    fn adds_a_patch_number_to_a_two_part_l4t_version() {
        let universe = device_contract(
            None,
            Some(&kernel("Linux", "4.9.140-l4t-r32.5-tegra")),
            None,
        );
        for ty in ["sw.l4t", "sw.kernel-bsp"] {
            let found = universe.find_children(&Matcher::new(ty));
            assert_eq!(found[0].get_version().as_deref(), Some("32.5.0"));
        }
    }

    #[test]
    fn the_contract_satisfies_l4t_requirements() {
        let universe = device_contract(None, Some(&kernel("Linux", "5.15.136-l4t-r36.4.3")), None);
        // `sw.l4t` requirements have no slug
        for (ty, slug) in [("sw.l4t", None), ("sw.kernel-bsp", Some("l4t"))] {
            let req = |version: &str| {
                let mut req = json!({ "type": ty, "version": version });
                if let Some(slug) = slug {
                    req["slug"] = json!(slug);
                }
                service(json!([req]))
            };
            assert!(universe.satisfies_child_contract(&req(">=36"), Some(&[ty])));
            assert!(!universe.satisfies_child_contract(&req(">=37"), Some(&[ty])));
        }
    }

    #[test]
    fn the_contract_satisfies_an_either_l4t_requirement() {
        let either = |version: &str| {
            service(json!([{ "or": [
                { "type": "sw.l4t", "version": version },
                { "type": "sw.kernel-bsp", "slug": "l4t", "version": version },
            ]}]))
        };
        let types = Some(&["sw.l4t", "sw.kernel-bsp"][..]);

        let l4t = device_contract(None, Some(&kernel("Linux", "5.15.136-l4t-r36.4.3")), None);
        assert!(l4t.satisfies_child_contract(&either(">=36"), types));
        assert!(!l4t.satisfies_child_contract(&either(">=37"), types));

        let other = device_contract(None, Some(&kernel("Linux", "5.15.148-yocto")), None);
        assert!(!other.satisfies_child_contract(&either(">=36"), types));
    }

    #[test]
    fn builds_the_supervisor_contracts() {
        let universe = device_contract(None, None, Some("v7.21.0"));
        for ty in ["sw.supervisor", "sw.application"] {
            let found = universe.find_children(&Matcher::new(ty));
            assert_eq!(found.len(), 1, "{ty}");
            assert_eq!(found[0].get_slug(), Some("balena-supervisor"));
            assert_eq!(
                found[0].get_version().map(|v| v.to_string()),
                Some("7.21.0".to_string())
            );
        }
    }

    #[test]
    fn omits_the_supervisor_contracts_without_a_supervisor() {
        let universe = device_contract(None, None, None);
        assert!(
            universe
                .find_children(&Matcher::new("sw.supervisor"))
                .is_empty()
        );
        assert!(
            universe
                .find_children(&Matcher::new("sw.application"))
                .is_empty()
        );
    }

    #[test]
    fn the_contract_satisfies_supervisor_requirements() {
        let universe = device_contract(None, None, Some("v7.21.0"));
        for ty in ["sw.supervisor", "sw.application"] {
            let svc = |version: &str| {
                service(json!([{ "type": ty, "slug": "balena-supervisor", "version": version }]))
            };
            assert!(universe.satisfies_child_contract(&svc(">=7.0.0"), None));
            assert!(!universe.satisfies_child_contract(&svc(">=8.0.0"), None));
        }
    }

    #[test]
    fn omits_the_l4t_contracts_for_other_kernels() {
        let universe = device_contract(None, Some(&kernel("Linux", "5.15.148-yocto")), None);
        assert!(universe.find_children(&Matcher::new("sw.l4t")).is_empty());
        assert!(
            universe
                .find_children(&Matcher::new("sw.kernel-bsp"))
                .is_empty()
        );
    }

    fn target_with_services(services: serde_json::Value) -> crate::models::DeviceTarget {
        serde_json::from_value(json!({
            "apps": {
                "my-app": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release": { "installed": true, "services": services }
                    }
                }
            }
        }))
        .unwrap()
    }

    fn target_with_contract(contract: Option<serde_json::Value>) -> crate::models::DeviceTarget {
        let mut service = json!({ "id": 1, "image": "alpine:latest" });
        if let Some(contract) = contract {
            service["contract"] = json!({ "contract": contract });
        }
        target_with_services(json!({ "main": service }))
    }

    /// A contract that a `6.x` device does not satisfy
    fn unmet_contract() -> serde_json::Value {
        json!({
            "type": "sw.container",
            "requires": [{ "type": "sw.os", "slug": "balena-os", "version": ">=7" }]
        })
    }

    fn reject(target: &mut crate::models::DeviceTarget, device_version: &str) -> Vec<String> {
        let universe = device_contract(Some(&os("balenaOS", Some(device_version))), None, None);
        reject_unsatisfied_releases(&mut target.apps, &universe)
    }

    fn app(target: &crate::models::DeviceTarget) -> &AppTarget {
        target.apps.get(&Uuid::from("my-app")).unwrap()
    }

    #[test]
    fn rejects_a_release_whose_service_requires_a_newer_os() {
        let mut target = target_with_contract(Some(json!({
            "type": "sw.container",
            "requires": [{ "type": "sw.os", "slug": "balena-os", "version": ">=7" }]
        })));

        let reasons = reject(&mut target, "6.5.6");

        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].starts_with("unmet requirement for service main of app my-app: "));
        assert!(app(&target).releases.is_empty());
        assert_eq!(
            app(&target).rejected_release,
            Some(Uuid::from("my-release"))
        );
    }

    #[test]
    fn accepts_a_release_whose_service_requirements_are_met() {
        let mut target = target_with_contract(Some(json!({
            "type": "sw.container",
            "requires": [{ "type": "sw.os", "slug": "balena-os", "version": ">=6" }]
        })));

        assert!(reject(&mut target, "6.5.6").is_empty());
        assert_eq!(app(&target).releases.len(), 1);
        assert_eq!(app(&target).rejected_release, None);
    }

    #[test]
    fn rejects_a_release_whose_service_requires_an_unsupported_type() {
        let mut target = target_with_contract(Some(json!({
            "type": "sw.container",
            "requires": [
                { "type": "sw.os", "slug": "balena-os", "version": ">=6" },
                { "type": "sw.supervisor", "version": ">=14.0.0" }
            ]
        })));

        let reasons = reject(&mut target, "6.5.6");
        println!("reasons: [{}]", reasons.join(", "));

        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("sw.supervisor") && !reasons[0].contains("sw.os"));
        assert!(app(&target).releases.is_empty());
        assert_eq!(
            app(&target).rejected_release,
            Some(Uuid::from("my-release"))
        );
    }

    #[test]
    fn leaves_services_without_a_contract_alone() {
        let mut target = target_with_contract(None);

        assert!(reject(&mut target, "6.5.6").is_empty());
        assert_eq!(app(&target).releases.len(), 1);
    }

    #[test]
    fn removes_the_contracts_from_the_target() {
        let mut target = target_with_contract(Some(json!({
            "type": "sw.container",
            "requires": [{ "type": "sw.os", "slug": "balena-os", "version": ">=6" }]
        })));
        let before = serde_json::to_value(&target).unwrap();
        assert!(before.to_string().contains("\"contract\""));

        reject(&mut target, "6.5.6");

        // a contract left on the target would never be on the current state, so
        // the planner would not be able to close the difference
        let after = serde_json::to_value(&target).unwrap();
        assert!(!after.to_string().contains("\"contract\""));
        assert_eq!(
            after,
            serde_json::to_value(target_with_contract(None)).unwrap()
        );
    }

    #[test]
    fn reports_every_service_that_does_not_satisfy_the_contract() {
        let unmet = unmet_contract();
        let mut target = target_with_services(json!({
            "one": { "id": 1, "image": "alpine:latest", "contract": { "contract": unmet } },
            "two": { "id": 2, "image": "alpine:latest", "contract": { "contract": unmet } },
            "three": { "id": 3, "image": "alpine:latest" }
        }));

        let reasons = reject(&mut target, "6.5.6");

        assert_eq!(reasons.len(), 2);
        assert!(reasons.iter().any(|r| r.contains("service one ")));
        assert!(reasons.iter().any(|r| r.contains("service two ")));
        assert!(app(&target).releases.is_empty());
        assert_eq!(
            app(&target).rejected_release,
            Some(Uuid::from("my-release"))
        );
    }

    #[test]
    fn removes_optional_services_that_do_not_satisfy_the_contract() {
        let unmet = unmet_contract();
        let mut target = target_with_services(json!({
            "opt": {
                "id": 1,
                "image": "alpine:latest",
                "contract": { "optional": true, "contract": unmet }
            },
            "main": { "id": 2, "image": "alpine:latest" }
        }));

        let reasons = reject(&mut target, "6.5.6");

        assert!(reasons.is_empty());
        let rel = app(&target)
            .releases
            .get(&Uuid::from("my-release"))
            .unwrap();
        assert!(rel.services.get("opt").is_none());
        assert!(rel.services.get("main").is_some());
        assert_eq!(app(&target).rejected_release, None);
    }

    #[test]
    fn rejects_the_release_if_a_required_service_is_unmet_next_to_an_optional_one() {
        let unmet = unmet_contract();
        let mut target = target_with_services(json!({
            "opt": {
                "id": 1,
                "image": "alpine:latest",
                "contract": { "optional": true, "contract": unmet }
            },
            "req": {
                "id": 2,
                "image": "alpine:latest",
                "contract": { "contract": unmet }
            }
        }));

        let reasons = reject(&mut target, "6.5.6");

        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("service req "));
        assert!(app(&target).releases.is_empty());
    }

    #[test]
    fn rejects_the_release_if_a_service_depends_on_an_unmet_optional_one() {
        let unmet = unmet_contract();
        let mut target = target_with_services(json!({
            "opt": {
                "id": 1,
                "image": "alpine:latest",
                "contract": { "optional": true, "contract": unmet }
            },
            "main": {
                "id": 2,
                "image": "alpine:latest",
                "depends_on": { "opt": { "condition": "service_started", "restart": false, "required": true } }
            }
        }));

        let reasons = reject(&mut target, "6.5.6");

        assert!(reasons.iter().any(|r| r.contains("required by: main")));
        assert!(app(&target).releases.is_empty());
        assert_eq!(
            app(&target).rejected_release,
            Some(Uuid::from("my-release"))
        );
    }

    #[test]
    fn rejects_the_release_if_a_non_required_dependant_remains() {
        let mut target = target_with_services(json!({
            "opt": {
                "id": 1,
                "image": "alpine:latest",
                "contract": { "optional": true, "contract": unmet_contract() }
            },
            "main": {
                "id": 2,
                "image": "alpine:latest",
                "depends_on": { "opt": { "condition": "service_started", "restart": false, "required": false } }
            }
        }));

        let reasons = reject(&mut target, "6.5.6");

        assert!(reasons.iter().any(|r| r.contains("required by: main")));
        assert!(app(&target).releases.is_empty());
    }

    #[test]
    fn removes_optional_services_that_depend_on_each_other() {
        let dep = json!({ "opt-a": { "condition": "service_started", "restart": false, "required": true } });
        let optional = |extra: serde_json::Value| {
            let mut svc = json!({
                "id": 1,
                "image": "alpine:latest",
                "contract": { "optional": true, "contract": unmet_contract() }
            });
            svc.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            svc
        };
        let mut target = target_with_services(json!({
            "opt-a": optional(json!({})),
            "opt-b": optional(json!({ "depends_on": dep })),
            "main": { "id": 3, "image": "alpine:latest" }
        }));

        assert!(reject(&mut target, "6.5.6").is_empty());
        let rel = app(&target)
            .releases
            .get(&Uuid::from("my-release"))
            .unwrap();
        assert_eq!(rel.services.len(), 1);
    }
}
