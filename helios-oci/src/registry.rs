use crate::Credentials;
use crate::util::http::Uri;
use crate::util::types::{ApiKey, ImageUri};

#[derive(Clone, Debug)]
pub struct RegistryAuth {
    api_endpoint: Uri,
    api_key: ApiKey,
}

impl RegistryAuth {
    pub fn new(api_endpoint: Uri, api_key: ApiKey) -> Self {
        Self {
            api_endpoint,
            api_key,
        }
    }

    /// Get credentials for the image URI
    ///
    /// The client will only return credentials if the remote API endpoint
    /// has a domain root that matches the URI registry root
    pub fn credentials(&self, image_uri: &ImageUri) -> Option<Credentials> {
        if let Some(registry_uri) = image_uri.registry().and_then(|r| r.parse::<Uri>().ok())
        && let Some(api_domain) = self.api_endpoint.domain()
        && let Some(img_domain) = registry_uri.domain()
        // only provide credentials for the URI if the hosts have matching roots
        && api_domain.root().is_some() && api_domain.root() == img_domain.root()
        {
            return Some(Credentials {
                // FIXME: this `d` username is very balena specific, we really should be using
                // `identitytoken`, but that does not work with the balena API
                username: Some("d".to_string()),
                password: Some(self.api_key.to_string()),
                ..Default::default()
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_auth(api_endpoint: &'static str) -> RegistryAuth {
        RegistryAuth::new(
            Uri::from_static(api_endpoint),
            ApiKey::from("secret-api-key".to_string()),
        )
    }

    fn image(uri: &str) -> ImageUri {
        ImageUri::from_static(uri)
    }

    // --- Positive cases ---

    #[test]
    fn credentials_returned_for_subdomain_of_same_root() {
        // api endpoint subdomain and image registry subdomain differ,
        // but they share the same registrable root domain
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("registry.balena-cloud.com/org/image:1.0");
        let creds = auth
            .credentials(&img)
            .expect("expected credentials for same-root domain");
        assert_eq!(creds.username, Some("d".to_string()));
        assert_eq!(creds.password, Some("secret-api-key".to_string()));
    }

    #[test]
    fn credentials_returned_for_registry_with_port() {
        // port number in the registry should not prevent credential matching
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("registry.balena-cloud.com:443/org/image:1.0");
        assert!(auth.credentials(&img).is_some());
    }

    #[test]
    fn credentials_returned_for_different_subdomains_same_root() {
        // both api and registry are under the same root despite different subdomains
        let auth = make_auth("https://api.staging.balena-cloud.com");
        let img = image("registry.staging.balena-cloud.com/org/image:1.0");
        assert!(auth.credentials(&img).is_some());
    }

    // --- Security: credentials must never leak to a different domain ---

    #[test]
    fn no_credentials_for_different_root_domain() {
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("docker.io/library/ubuntu:20.04");
        assert!(auth.credentials(&img).is_none());
    }

    #[test]
    fn no_credentials_for_image_without_registry() {
        // images without an explicit registry have no host to match against
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("ubuntu:20.04");
        assert!(auth.credentials(&img).is_none());
    }

    #[test]
    fn no_credentials_for_domain_suffix_lookalike() {
        // attacker registers balena-cloud.com.evil.com — the root is evil.com, not balena-cloud.com
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("registry.balena-cloud.com.evil.com/image:1.0");
        assert!(auth.credentials(&img).is_none());
    }

    #[test]
    fn no_credentials_for_different_tld() {
        // same second-level label but different TLD must not match
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("registry.balena-cloud.org/image:1.0");
        assert!(auth.credentials(&img).is_none());
    }

    #[test]
    fn no_credentials_for_domain_with_shared_suffix() {
        // mybalena-cloud.com shares ".com" but not the registrable root with balena-cloud.com
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("registry.mybalena-cloud.com/image:1.0");
        assert!(auth.credentials(&img).is_none());
    }

    #[test]
    fn no_credentials_for_localhost_registry() {
        // localhost has no registrable root, so it can never match a real domain
        let auth = make_auth("https://api.balena-cloud.com");
        let img = image("localhost:5000/myimage:latest");
        assert!(auth.credentials(&img).is_none());
    }
}
