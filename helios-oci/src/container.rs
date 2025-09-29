use std::collections::HashMap;

use bollard::{query_parameters::ListContainersOptions, secret::ContainerSummary};

use super::{Client, Error, Result};

#[derive(Debug, Clone)]
pub struct Container<'a>(&'a Client);

impl<'a> Container<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self(client)
    }
}

impl Container<'_> {
    /// Returns an iterator on the list of images on the server.
    ///
    /// Note that it uses a different, smaller representation of an image than
    /// inspecting a single image.
    pub async fn list_with_labels(&self, labels: Vec<&str>) -> Result<List> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            labels.into_iter().map(|s| s.to_owned()).collect(),
        );

        let opts = ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        };

        let res = self.0.inner().list_containers(Some(opts)).await;
        let containers = res.map_err(Error::with_context("failed to list containers"))?;

        Ok(List(containers))
    }
}

// by ref in order to clone only what's necessary to build LocalImage.
impl<'a> From<&'a ContainerSummary> for LocalContainer {
    fn from(value: &'a ContainerSummary) -> Self {
        let id = value.id.clone().expect("container ID should not be nil");
        let image_id = value
            .image_id
            .clone()
            .expect("container image ID should not be nil");

        let labels = value.labels.clone().unwrap_or_default();
        Self {
            id,
            image_id,
            labels,
        }
    }
}

#[derive(Debug, Clone)]
pub struct List(Vec<ContainerSummary>);

type ListItem = (String, LocalContainer);

impl List {
    pub fn iter(&self) -> impl Iterator<Item = ListItem> + '_ {
        self.0
            .iter()
            .flat_map(|container| container.names.iter().map(move |name| (name, container)))
            .flat_map(|(names, container)| {
                names.iter().map(|name| (name.clone(), container.into()))
            })
    }
}

#[derive(Debug, Clone)]
pub struct LocalContainer {
    /// The engine id of the container
    pub id: String,

    /// The content-addressable image id
    pub image_id: String,

    /// User-defined key/value metadata.
    pub labels: HashMap<String, String>,
}
