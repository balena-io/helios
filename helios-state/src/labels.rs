/// Label indicating a managed resource
pub(crate) const LABEL_SUPERVISED: &str = "io.balena.supervised";

/// Label storing the app UUID on managed resources
pub(crate) const LABEL_APP_UUID: &str = "io.balena.app-uuid";

/// Label storing the service name on managed resources
pub(crate) const LABEL_SERVICE_NAME: &str = "io.balena.service-name";

/// Label storing the network name on managed resources
pub(crate) const LABEL_NETWORK_NAME: &str = "io.balena.network-name";

/// Label storing the volume name on managed resources
pub(crate) const LABEL_VOLUME_NAME: &str = "io.balena.volume-name";

/// Label storing the service id on managed resources
pub(crate) const LABEL_SERVICE_ID: &str = "io.balena.service-id";

/// Label storing the JSON-encoded `depends_on` map for a service container.
pub(crate) const LABEL_DEPENDS_ON: &str = "io.balena.private.depends-on";
