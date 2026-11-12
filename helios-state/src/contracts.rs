//! The device contract, describing what the device provides to the services it runs

use contrato::{Contract, Universe};
use mahler::state::{Map, State};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use crate::common_types::{OperatingSystem, Uuid};
use crate::models::AppTarget;

/// A service contract, wrapped to make it usable on a state model
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceContract(Contract);

impl State for ServiceContract {
    type Target = Self;
}

impl From<Contract> for ServiceContract {
    fn from(contract: Contract) -> Self {
        Self(contract)
    }
}

/// Reject the app releases with services the device contract does not satisfy, and say why
///
/// A reason is given for every unmet requirement.
///
/// A rejected app is left with no releases and a `rejected_release`, the same as an app whose
/// target could not be parsed. A requirement on a type the device contract does not describe
/// is not met. All service contracts are removed from the target, as they are not needed to
/// plan.
pub(crate) fn reject_unsatisfied_releases(
    apps: &mut Map<Uuid, AppTarget>,
    contract: &Universe,
) -> Vec<String> {
    let mut reasons = Vec::new();

    for (app_uuid, app) in apps.iter_mut() {
        let mut rejected = None;

        for (rel_uuid, rel) in app.releases.iter_mut() {
            for (svc_name, svc) in rel.services.iter_mut() {
                let Some(ServiceContract(svc_contract)) = svc.contract.take() else {
                    continue;
                };
                let unmet = contract.get_not_satisfied_child_requirements(&svc_contract, None);
                if !unmet.is_empty() {
                    reasons.extend(unmet.iter().map(|req| {
                        format!(
                            "unmet requirement for service {svc_name} of app {app_uuid}: {}",
                            serde_json::to_string(req).unwrap_or_default()
                        )
                    }));
                    rejected.get_or_insert_with(|| rel_uuid.clone());
                }
            }
        }

        if let Some(rel_uuid) = rejected {
            app.releases.clear();
            app.rejected_release = Some(rel_uuid);
        }
    }

    reasons
}

/// Build the device contract from what is known about the device
///
/// Without a known OS, or one that cannot be expressed in a contract (e.g.
/// the name is not a valid slug), the contract has no `sw.os`.
pub(crate) fn device_contract(os: Option<&OperatingSystem>) -> Universe {
    let mut universe = Universe::new();

    if let Some(os) = os
        && let Err(e) = os_contract(os).and_then(|contract| universe.add_child(contract).map(drop))
    {
        // the device then fails to satisfy any `sw.os` requirement
        warn!("could not build the `sw.os` contract for '{os}': {e}");
    }

    universe
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
        let universe = device_contract(Some(&os("balenaOS", Some("6.5.6+rev1"))));
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_slug(), Some("balena-os"));
        assert_eq!(found[0].get_version().as_deref(), Some("6.5.6+rev1"));
    }

    #[test]
    fn builds_the_os_contract_without_version() {
        let universe = device_contract(Some(&os("balenaOS", None)));
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_version(), None);
    }

    #[test]
    fn leaves_the_contract_empty_without_os() {
        let universe = device_contract(None);
        assert!(universe.find_children(&Matcher::new("sw.os")).is_empty());
    }

    #[test]
    fn omits_the_os_if_the_name_is_empty() {
        assert!(!has_os(&device_contract(Some(&os("", Some("1.0.0"))))));
        assert!(!has_os(&device_contract(Some(&os("  ", Some("1.0.0"))))));
    }

    #[test]
    fn omits_the_os_if_the_name_does_not_start_with_a_letter() {
        assert!(!has_os(&device_contract(Some(&os("1OS", Some("1.0.0"))))));
        assert!(!has_os(&device_contract(Some(&os("+os", Some("1.0.0"))))));
        assert!(!has_os(&device_contract(Some(&os("!!!", Some("1.0.0"))))));
    }

    #[test]
    fn replaces_characters_a_slug_does_not_allow() {
        assert_eq!(slugify("Debian GNU/Linux"), "debian-gnu-linux");
        assert_eq!(slugify("Foo (bar)"), "foo-bar");
        assert_eq!(slugify("balenaOS!"), "balena-os");
        assert_eq!(slugify("_os"), "os");
        assert_eq!(slugify("éOS"), "os");
        assert_eq!(slugify("a - b"), "a-b");

        let universe = device_contract(Some(&os("Debian GNU/Linux", Some("12"))));
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found[0].get_slug(), Some("debian-gnu-linux"));
    }

    #[test]
    fn accepts_dots_and_plus_in_the_name() {
        let universe = device_contract(Some(&os("os.v2+x", Some("1.0.0"))));
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found[0].get_slug(), Some("os.v2+x"));
    }

    #[test]
    fn a_version_that_is_not_semver_does_not_fail() {
        let universe = device_contract(Some(&os("balenaOS", Some("wheezy"))));
        let found = universe.find_children(&Matcher::new("sw.os"));
        assert_eq!(found[0].get_version().as_deref(), Some("wheezy"));
    }

    #[test]
    fn the_contract_satisfies_os_requirements() {
        let universe = device_contract(Some(&os("balenaOS", Some("6.5.6"))));
        let svc = service(json!([{ "type": "sw.os", "slug": "balena-os", "version": ">=6" }]));
        assert!(universe.satisfies_child_contract(&svc, Some(&["sw.os"])));

        let svc = service(json!([{ "type": "sw.os", "slug": "balena-os", "version": ">=7" }]));
        assert!(!universe.satisfies_child_contract(&svc, Some(&["sw.os"])));
    }

    fn target_with_contract(contract: Option<serde_json::Value>) -> crate::models::DeviceTarget {
        let mut service = json!({ "id": 1, "image": "alpine:latest" });
        if let Some(contract) = contract {
            service["contract"] = contract;
        }
        serde_json::from_value(json!({
            "apps": {
                "my-app": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release": { "installed": true, "services": { "main": service } }
                    }
                }
            }
        }))
        .unwrap()
    }

    fn reject(target: &mut crate::models::DeviceTarget, device_version: &str) -> Vec<String> {
        let universe = device_contract(Some(&os("balenaOS", Some(device_version))));
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
        let unmet = json!({
            "type": "sw.container",
            "requires": [{ "type": "sw.os", "slug": "balena-os", "version": ">=7" }]
        });
        let mut target: crate::models::DeviceTarget = serde_json::from_value(json!({
            "apps": {
                "my-app": {
                    "id": 1,
                    "name": "my-app",
                    "releases": {
                        "my-release": {
                            "installed": true,
                            "services": {
                                "one": { "id": 1, "image": "alpine:latest", "contract": unmet },
                                "two": { "id": 2, "image": "alpine:latest", "contract": unmet },
                                "three": { "id": 3, "image": "alpine:latest" }
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

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
}
