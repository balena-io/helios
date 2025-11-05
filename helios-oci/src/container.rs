use std::collections::HashMap;

use bollard::{
    models::ContainerCreateBody,
    query_parameters::{
        CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
        RemoveContainerOptions,
    },
    secret::ContainerSummary,
};
use futures_lite::StreamExt;

use super::util::types::ImageUri;
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

    /// Create a temporary container from the given image
    ///
    /// This is only meant to get access to the container files and not to be started
    pub async fn create_tmp(&self, name: &str, image: &ImageUri) -> Result<String> {
        let options = Some(CreateContainerOptions {
            name: Some(name.to_owned()),
            platform: String::from(""),
        });

        let config = ContainerCreateBody {
            image: Some(image.to_string()),
            cmd: Some(vec!["/bin/false".to_string()]),
            ..Default::default()
        };

        let res = self.0.inner().create_container(options, config).await?;

        Ok(res.id)
    }

    /// Remove a stopped container
    pub async fn remove(&self, container_name: &str) -> Result<()> {
        if let Err(e) = self
            .0
            .inner()
            .remove_container(container_name, None::<RemoveContainerOptions>)
            .await
        {
            if let bollard::errors::Error::DockerResponseServerError { status_code, .. } = e {
                // do not fail if the container doesn't exist
                if status_code != 404 {
                    return Err(Error::from(e)
                        .context(format!("failed to remove container {container_name}")));
                }
            } else {
                return Err(
                    Error::from(e).context(format!("failed to remove container {container_name}"))
                );
            }
        }
        Ok(())
    }

    /// Reads a container directory contents into an array of bytes using tar representation
    pub async fn read_from(&self, container_name: &str, container_path: &str) -> Result<Vec<u8>> {
        let mut stream = self.0.inner().download_from_container(
            container_name,
            Some(DownloadFromContainerOptions {
                path: container_path.to_owned(),
            }),
        );

        let mut archive = Vec::new();
        while let Some(res) = stream.next().await {
            match res {
                Ok(chunk) => archive.extend_from_slice(&chunk),
                Err(e) => {
                    return Err(Error::from(e).context(format!(
                        "failed to read {container_path} from container {container_name}"
                    )));
                }
            }
        }

        Ok(archive)
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
